use super::*;

#[cfg(test)]
use crate::daemon::workspace_route_handles::WorkspacePrimaryBranchRefreshEffect;
#[cfg(test)]
use crate::daemon::workspace_stream_route_handles::WorkspaceVcsStreamRefreshEffect;

impl workspace_deps::WorkspaceRouteDeps {
    pub fn workspace_registry(&self) -> WorkspaceRegistryHandle {
        WorkspaceRegistryHandle::new(
            self.global_store.clone(),
            self.workspace_store_lookup(),
            self.telemetry.clone(),
        )
    }
    pub fn workspace_deletion(&self) -> WorkspaceDeletionHandle {
        WorkspaceDeletionHandle::new(Arc::new(
            crate::daemon::workspaces::WorkspaceDeletionRuntime::new(
                WorkspaceDeletionRuntimeDeps {
                    data_root: self.data_root.clone(),
                    daemon_url: self.daemon_url.clone(),
                    stores: self.stores.clone(),
                    global_store: self.global_store.clone(),
                    sessions: Arc::clone(&self.sessions),
                    active_snapshot: Arc::clone(&self.active_snapshot),
                    workspace_active_snapshot_cache: Arc::clone(
                        &self.workspace_active_snapshot_cache,
                    ),
                    workspace_active_heads_cache: Arc::clone(&self.workspace_active_heads_cache),
                    workspace_file_completions_cache: Arc::clone(
                        &self.workspace_file_completions_cache,
                    ),
                    harness: Arc::clone(&self.harness),
                    providers: Arc::clone(&self.providers),
                    merge_queue: Arc::clone(&self.merge_queue),
                },
            ),
        ))
    }
    pub fn workspace_merge_queue_config(&self) -> WorkspaceMergeQueueConfigHandle {
        WorkspaceMergeQueueConfigHandle::new(
            self.workspace_store_lookup(),
            Arc::clone(&self.merge_queue),
        )
    }
    pub fn workspace_attachments(&self) -> WorkspaceAttachmentsHandle {
        WorkspaceAttachmentsHandle::new(
            self.global_store.clone(),
            self.workspace_store_lookup(),
            self.workspace_attachments_runtime(),
        )
    }
    pub fn workspace_primary_branch(&self) -> WorkspacePrimaryBranchHandle {
        let vcs_runtime = self.worktree_vcs_runtime_host();
        let vcs_execution = self.worktree_vcs_execution_host();
        let refresh_vcs_snapshot = Arc::new({
            let vcs_runtime = vcs_runtime.clone();
            let vcs_execution = vcs_execution.clone();
            move |worktree: Worktree| {
                let vcs_runtime = vcs_runtime.clone();
                let vcs_execution = vcs_execution.clone();
                Box::pin(async move {
                    crate::daemon::git_status::emit_worktree_vcs_snapshot_for_worktree(
                        &vcs_runtime,
                        &vcs_execution,
                        &worktree,
                        true,
                    )
                    .await
                }) as WorkspacePrimaryBranchRefreshFuture
            }
        });
        WorkspacePrimaryBranchHandle::new(
            self.global_store.clone(),
            self.workspace_store_lookup(),
            refresh_vcs_snapshot,
        )
    }
    #[cfg(test)]
    pub(in crate::daemon) fn workspace_primary_branch_with_refresh_effect(
        &self,
        refresh_vcs_snapshot: WorkspacePrimaryBranchRefreshEffect,
    ) -> WorkspacePrimaryBranchHandle {
        WorkspacePrimaryBranchHandle::new(
            self.global_store.clone(),
            self.workspace_store_lookup(),
            refresh_vcs_snapshot,
        )
    }
    pub fn workspace_org_policy(&self) -> WorkspaceOrgPolicyHandle {
        WorkspaceOrgPolicyHandle::new(self.global_store.clone(), self.workspace_store_lookup())
    }
    pub fn workspace_prompt_bootstrap_config(&self) -> WorkspacePromptBootstrapConfigHandle {
        WorkspacePromptBootstrapConfigHandle::new(self.workspace_store_lookup())
    }
    pub fn workspace_file_completions(&self) -> WorkspaceFileCompletionsHandle {
        WorkspaceFileCompletionsHandle::new(
            self.global_store.clone(),
            Arc::clone(&self.workspace_file_completions_cache),
            self.perf_telemetry.clone(),
        )
    }
    pub fn workspace_execution_config(&self) -> WorkspaceExecutionConfigHandle {
        WorkspaceExecutionConfigHandle::new(
            self.global_store.clone(),
            self.workspace_store_lookup(),
            self.data_root.clone(),
        )
    }
    pub fn workspace_harness_container(&self) -> WorkspaceHarnessContainerHandle {
        WorkspaceHarnessContainerHandle::new(
            self.global_store.clone(),
            self.workspace_store_lookup(),
            self.daemon_url.clone(),
            Arc::clone(&self.harness),
        )
    }
    pub fn workspace_worktree(&self) -> WorkspaceWorktreeHandle {
        WorkspaceWorktreeHandle::new(
            self.global_store.clone(),
            self.workspace_store_lookup(),
            self.data_root.clone(),
        )
    }
    pub(super) fn workspace_provider_model_preferences_with_provider_routes(
        &self,
        provider_routes: &provider_deps::ProviderRouteDeps,
    ) -> WorkspaceProviderModelPreferenceHandle {
        WorkspaceProviderModelPreferenceHandle::new(
            provider_routes.provider_workspace_launch_runtime(),
        )
    }
    pub fn workspace_active(&self) -> WorkspaceActiveHandle {
        let hydration = WorkspaceActiveHydrationRuntime::new(
            self.global_store.clone(),
            self.workspace_store_lookup(),
            Arc::clone(&self.active_snapshot),
        );
        let cache = WorkspaceActiveCacheRuntime::new(
            Arc::clone(&self.workspace_active_snapshot_cache),
            Arc::clone(&self.workspace_active_heads_cache),
        );
        let merge_queue = self.merge_queue_route_host();
        let ensure_workspace_active_snapshot_hydrated = Arc::new({
            let hydration = hydration.clone();
            move |workspace_id: WorkspaceId| {
                let hydration = hydration.clone();
                Box::pin(async move {
                    hydration
                        .ensure_workspace_active_snapshot_hydrated(workspace_id)
                        .await
                }) as WorkspaceActiveFuture<_>
            }
        });
        let activate_workspace_merge_queue = Arc::new({
            let merge_queue = Arc::clone(&merge_queue);
            move |workspace_id: WorkspaceId| {
                let merge_queue = Arc::clone(&merge_queue);
                Box::pin(async move {
                    ctx_merge_queue::activate_workspace_merge_queue(&merge_queue, workspace_id)
                        .await;
                }) as WorkspaceActiveFuture<_>
            }
        });
        let cache_workspace_active_snapshot = Arc::new({
            let cache = cache.clone();
            move |snapshot: WorkspaceActiveSnapshot| {
                let cache = cache.clone();
                Box::pin(async move {
                    cache.cache_workspace_active_snapshot(snapshot).await;
                }) as WorkspaceActiveFuture<_>
            }
        });
        let cache_workspace_active_heads = Arc::new({
            let cache = cache.clone();
            move |heads: WorkspaceActiveHeadBatch| {
                let cache = cache.clone();
                Box::pin(async move {
                    cache.cache_workspace_active_heads(heads).await;
                }) as WorkspaceActiveFuture<_>
            }
        });
        WorkspaceActiveHandle::new(WorkspaceActiveHandleParts {
            active_snapshot: Arc::clone(&self.active_snapshot),
            effects: WorkspaceActiveEffects::new(WorkspaceActiveEffectsParts {
                ensure_workspace_active_snapshot_hydrated,
                activate_workspace_merge_queue,
                cache_workspace_active_snapshot,
                cache_workspace_active_heads,
            }),
        })
    }
    pub fn workspace_stream(&self) -> WorkspaceStreamHandle {
        let workspace_stores = self.workspace_store_lookup();
        let session_stores = self.session_store_lookup();
        let hydration = WorkspaceActiveHydrationRuntime::new(
            self.global_store.clone(),
            workspace_stores.clone(),
            Arc::clone(&self.active_snapshot),
        );
        let merge_queue = self.merge_queue_route_host();
        let lifecycle_host = Arc::new(WorkspaceStreamSessionLifecycleHost::new(
            self.global_store.clone(),
            Arc::clone(&self.active_snapshot),
            Arc::clone(&self.providers),
        ));
        let ensure_workspace_active_snapshot_hydrated = Arc::new({
            let hydration = hydration.clone();
            move |workspace_id: WorkspaceId| {
                let hydration = hydration.clone();
                Box::pin(async move {
                    hydration
                        .ensure_workspace_active_snapshot_hydrated(workspace_id)
                        .await
                }) as WorkspaceStreamFuture<_>
            }
        });
        let activate_workspace_merge_queue = Arc::new({
            let merge_queue = Arc::clone(&merge_queue);
            move |workspace_id: WorkspaceId| {
                let merge_queue = Arc::clone(&merge_queue);
                Box::pin(async move {
                    ctx_merge_queue::activate_workspace_merge_queue(&merge_queue, workspace_id)
                        .await;
                }) as WorkspaceStreamFuture<_>
            }
        });
        WorkspaceStreamHandle::new(WorkspaceStreamHandleParts {
            global_store: self.global_store.clone(),
            workspace_stores,
            session_stores,
            active_snapshot: Arc::clone(&self.active_snapshot),
            sessions: Arc::clone(&self.sessions),
            lifecycle_host,
            telemetry: self.telemetry.clone(),
            perf_telemetry: self.perf_telemetry.clone(),
            effects: WorkspaceStreamEffects::new(WorkspaceStreamEffectsParts {
                ensure_workspace_active_snapshot_hydrated,
                activate_workspace_merge_queue,
            }),
        })
    }
    pub fn workspace_vcs_stream(&self) -> WorkspaceVcsStreamHandle {
        let vcs_runtime = self.worktree_vcs_runtime_host();
        let vcs_execution = self.worktree_vcs_execution_host();
        let ensure_worktree_vcs_watcher = Arc::new({
            let vcs_runtime = vcs_runtime.clone();
            let vcs_execution = vcs_execution.clone();
            move |worktree: Worktree| {
                let vcs_runtime = vcs_runtime.clone();
                let vcs_execution = vcs_execution.clone();
                Box::pin(async move {
                    vcs_runtime
                        .ensure_git_status_watcher(vcs_execution, worktree)
                        .await;
                }) as WorkspaceVcsStreamWatcherFuture
            }
        });
        let refresh_worktree_vcs = Arc::new({
            let vcs_runtime = vcs_runtime.clone();
            let vcs_execution = vcs_execution.clone();
            move |worktree: Worktree, summary: bool, touched_files: bool| {
                let vcs_runtime = vcs_runtime.clone();
                let vcs_execution = vcs_execution.clone();
                Box::pin(async move {
                    vcs_runtime
                        .ensure_git_status_watcher(vcs_execution.clone(), worktree.clone())
                        .await;
                    crate::daemon::git_status::request_worktree_vcs_refresh_without_transient(
                        &vcs_runtime,
                        &vcs_execution,
                        &worktree,
                        summary,
                        touched_files,
                    )
                    .await
                }) as WorkspaceVcsStreamRefreshFuture
            }
        });
        WorkspaceVcsStreamHandle::new(
            self.global_store.clone(),
            self.workspace_store_lookup(),
            self.workspace_vcs_stream_runtime.clone(),
            self.perf_telemetry.clone(),
            ensure_worktree_vcs_watcher,
            refresh_worktree_vcs,
        )
    }
    #[cfg(test)]
    pub(in crate::daemon) fn workspace_vcs_stream_with_refresh_effect(
        &self,
        refresh_worktree_vcs: WorkspaceVcsStreamRefreshEffect,
    ) -> WorkspaceVcsStreamHandle {
        let vcs_runtime = self.worktree_vcs_runtime_host();
        let vcs_execution = self.worktree_vcs_execution_host();
        let ensure_worktree_vcs_watcher = Arc::new({
            let vcs_runtime = vcs_runtime.clone();
            let vcs_execution = vcs_execution.clone();
            move |worktree: Worktree| {
                let vcs_runtime = vcs_runtime.clone();
                let vcs_execution = vcs_execution.clone();
                Box::pin(async move {
                    vcs_runtime
                        .ensure_git_status_watcher(vcs_execution, worktree)
                        .await;
                }) as WorkspaceVcsStreamWatcherFuture
            }
        });
        WorkspaceVcsStreamHandle::new(
            self.global_store.clone(),
            self.workspace_store_lookup(),
            self.workspace_vcs_stream_runtime.clone(),
            self.perf_telemetry.clone(),
            ensure_worktree_vcs_watcher,
            refresh_worktree_vcs,
        )
    }
}
