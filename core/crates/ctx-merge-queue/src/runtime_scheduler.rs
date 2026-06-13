use super::*;

pub fn spawn_merge_queue_runner<H: MergeQueueHost>(state: Arc<H>) {
    tokio::spawn(async move {
        let Some(mut rx) = H::merge_queue_runtime(state.as_ref())
            .take_schedule_rx()
            .await
        else {
            tracing::warn!("merge queue runner already started");
            return;
        };
        while let Some(workspace_id) = rx.recv().await {
            schedule_workspace_drain(&state, workspace_id).await;
        }
    });
}

pub async fn schedule_workspace_if_enabled_and_queued<H: MergeQueueHost>(
    state: &Arc<H>,
    workspace_id: WorkspaceId,
) -> Result<bool> {
    let store = H::raw_workspace_store(state.as_ref(), workspace_id).await?;
    schedule_store_if_enabled_and_queued(
        H::merge_queue_runtime(state.as_ref()),
        &store,
        workspace_id,
    )
    .await
}

pub async fn schedule_store_if_enabled_and_queued(
    runtime: &MergeQueueRuntime,
    store: &ctx_store::Store,
    workspace_id: WorkspaceId,
) -> Result<bool> {
    let cfg = load_merge_queue_config(store).await?;
    if !cfg.enabled || store.list_queued_merge_queue_entries().await?.is_empty() {
        return Ok(false);
    }
    runtime.schedule(workspace_id);
    Ok(true)
}

pub async fn activate_workspace_merge_queue<H: MergeQueueHost>(
    state: &Arc<H>,
    workspace_id: WorkspaceId,
) {
    let activate_start = std::time::Instant::now();
    let raw_store_start = std::time::Instant::now();
    let store = match H::raw_workspace_store(state.as_ref(), workspace_id).await {
        Ok(store) => store,
        Err(err) => {
            tracing::warn!(
                workspace_id = %workspace_id.0,
                "failed to activate merge queue for opened workspace: {err:#}"
            );
            return;
        }
    };
    let raw_store_ms = raw_store_start.elapsed().as_millis();
    let load_cfg_start = std::time::Instant::now();
    let cfg = match load_merge_queue_config(&store).await {
        Ok(cfg) => cfg,
        Err(err) => {
            tracing::warn!(
                workspace_id = %workspace_id.0,
                "failed to load merge queue config for opened workspace: {err:#}"
            );
            return;
        }
    };
    let load_cfg_ms = load_cfg_start.elapsed().as_millis();
    let list_queued_start = std::time::Instant::now();
    let queued = match store.list_queued_merge_queue_entries().await {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(
                workspace_id = %workspace_id.0,
                "failed to list queued merge queue entries for opened workspace: {err:#}"
            );
            return;
        }
    };
    let list_queued_ms = list_queued_start.elapsed().as_millis();
    if std::env::var_os("CTX_DEBUG_WORKSPACE_STREAM_TIMINGS").is_some() {
        eprintln!(
            "CTX_WS_TIMING merge_queue_activate workspace_id={} raw_store_ms={} load_cfg_ms={} list_queued_ms={} queued_entries={} enabled={} total_ms={}",
            workspace_id.0,
            raw_store_ms,
            load_cfg_ms,
            list_queued_ms,
            queued.len(),
            cfg.enabled,
            activate_start.elapsed().as_millis(),
        );
    }
    if queued.is_empty() {
        return;
    }
    if !cfg.enabled {
        if let Err(err) =
            cancel_queued_entries_for_disabled_workspace(state, &store, workspace_id).await
        {
            tracing::warn!(
                workspace_id = %workspace_id.0,
                "failed to cancel queued merge queue entries for disabled opened workspace: {err:#}"
            );
        }
        return;
    }
    let schedule_start = std::time::Instant::now();
    if let Err(err) = schedule_workspace_if_enabled_and_queued(state, workspace_id).await {
        tracing::warn!(
            workspace_id = %workspace_id.0,
            "failed to activate merge queue for opened workspace: {err:#}"
        );
    } else if std::env::var_os("CTX_DEBUG_WORKSPACE_STREAM_TIMINGS").is_some() {
        eprintln!(
            "CTX_WS_TIMING merge_queue_schedule workspace_id={} schedule_ms={}",
            workspace_id.0,
            schedule_start.elapsed().as_millis(),
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceDrainStep {
    Continue,
    Idle,
    Disabled,
    MissingWorkspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceDrainStop {
    Idle,
    Disabled,
    MissingWorkspace,
    Error,
}

pub async fn begin_workspace_drain<H: MergeQueueHost>(
    state: &H,
    workspace_id: WorkspaceId,
) -> bool {
    H::merge_queue_runtime(state)
        .begin_workspace_drain(workspace_id)
        .await
}

pub async fn finish_workspace_drain<H: MergeQueueHost>(
    state: &H,
    workspace_id: WorkspaceId,
) -> bool {
    H::merge_queue_runtime(state)
        .finish_workspace_drain(workspace_id)
        .await
}

pub async fn schedule_workspace_drain<H: MergeQueueHost>(
    state: &Arc<H>,
    workspace_id: WorkspaceId,
) {
    let should_spawn = begin_workspace_drain(state.as_ref(), workspace_id).await;
    if !should_spawn {
        return;
    }
    let state = Arc::clone(state);
    tokio::spawn(async move {
        let stop = loop {
            let step = match run_next_entry_for_workspace(&state, workspace_id).await {
                Ok(step) => step,
                Err(err) => {
                    tracing::warn!(workspace_id = %workspace_id.0, "merge queue runner error: {err:#}");
                    break WorkspaceDrainStop::Error;
                }
            };
            match step {
                WorkspaceDrainStep::Continue => continue,
                WorkspaceDrainStep::Idle => break WorkspaceDrainStop::Idle,
                WorkspaceDrainStep::Disabled => break WorkspaceDrainStop::Disabled,
                WorkspaceDrainStep::MissingWorkspace => break WorkspaceDrainStop::MissingWorkspace,
            }
        };

        if finish_workspace_drain(state.as_ref(), workspace_id).await {
            H::merge_queue_runtime(state.as_ref()).schedule(workspace_id);
            return;
        }

        let _ = reschedule_workspace_after_drain(&state, workspace_id, stop).await;
    });
}

pub async fn reschedule_workspace_after_drain<H: MergeQueueHost>(
    state: &Arc<H>,
    workspace_id: WorkspaceId,
    stop: WorkspaceDrainStop,
) -> bool {
    if stop == WorkspaceDrainStop::MissingWorkspace {
        return false;
    }
    if stop == WorkspaceDrainStop::Disabled {
        tracing::debug!(
            workspace_id = %workspace_id.0,
            "merge queue drain dormant because workspace queue is disabled"
        );
        return false;
    }
    match schedule_workspace_if_enabled_and_queued(state, workspace_id).await {
        Ok(true) => true,
        Ok(false) => false,
        Err(err) => {
            tracing::warn!(
                workspace_id = %workspace_id.0,
                "failed to re-check queued merge queue entries after drain: {err:#}"
            );
            false
        }
    }
}

async fn run_next_entry_for_workspace<H: MergeQueueHost>(
    state: &Arc<H>,
    workspace_id: WorkspaceId,
) -> Result<WorkspaceDrainStep> {
    let workspace = match H::get_workspace(state.as_ref(), workspace_id).await? {
        Some(workspace) => workspace,
        None => return Ok(WorkspaceDrainStep::MissingWorkspace),
    };
    let store = H::raw_workspace_store(state.as_ref(), workspace.id).await?;
    let cfg = load_merge_queue_config(&store).await?;
    if !cfg.enabled {
        cancel_queued_entries_for_disabled_workspace(state, &store, workspace.id).await?;
        return Ok(WorkspaceDrainStep::Disabled);
    }

    let Some(mut entry) = list_queued_entries_for_workspace(state.as_ref(), workspace_id)
        .await?
        .into_iter()
        .next()
    else {
        return Ok(WorkspaceDrainStep::Idle);
    };

    let now = Utc::now();
    let claimed = store.claim_merge_queue_entry(entry.id, now).await?;
    if !claimed {
        return Ok(WorkspaceDrainStep::Continue);
    }
    entry.status = MergeQueueEntryStatus::Running;
    entry.updated_at = now;
    run_entry(state, &workspace, entry, &cfg).await?;
    Ok(WorkspaceDrainStep::Continue)
}

pub async fn cancel_queued_entries_for_disabled_workspace<H: MergeQueueHost>(
    state: &Arc<H>,
    store: &ctx_store::Store,
    workspace_id: WorkspaceId,
) -> Result<()> {
    cancel_store_queued_entries_for_disabled_workspace(
        H::merge_queue_runtime(state.as_ref()),
        store,
        workspace_id,
    )
    .await
}

pub async fn cancel_store_queued_entries_for_disabled_workspace(
    runtime: &MergeQueueRuntime,
    store: &ctx_store::Store,
    workspace_id: WorkspaceId,
) -> Result<()> {
    let cfg = load_merge_queue_config(store).await?;
    if cfg.enabled {
        tracing::debug!(
            workspace_id = %workspace_id.0,
            cancelled = false,
            "skipped queued merge queue cancellation because the workspace queue was re-enabled"
        );
        return Ok(());
    }

    let queued = store.list_queued_merge_queue_entries().await?;
    if queued.is_empty() {
        return Ok(());
    }

    let now = Utc::now();
    for mut entry in queued {
        entry.status = MergeQueueEntryStatus::Cancelled;
        entry.error_message = Some("merge queue disabled while entry was queued".to_string());
        entry.updated_at = now;
        store.update_merge_queue_entry(&entry).await?;
    }

    tracing::debug!(
        workspace_id = %workspace_id.0,
        cancelled = true,
        "cancelled queued merge queue entries because the workspace queue is disabled"
    );
    runtime.notify_waiters();
    Ok(())
}
