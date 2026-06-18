use super::*;

impl session_deps::SessionRouteDeps {
    pub fn title_generation_local(&self) -> TitleGenerationLocalHandle {
        TitleGenerationLocalHandle::new(
            self.data_root.clone(),
            TitleGenerationLocalInstallEffect::new(
                self.data_root.clone(),
                Arc::clone(&self.providers),
                self.ops_events.clone(),
            ),
        )
    }
    pub fn demo_seed_transcript(&self) -> DemoSeedTranscriptHandle {
        DemoSeedTranscriptHandle::new(
            self.session_store_lookup(),
            self.workspace_store_lookup(),
            Arc::clone(&self.sessions),
            Arc::clone(&self.active_snapshot),
        )
    }
    pub(super) fn session_control_with_provider_routes(
        &self,
        provider_routes: &provider_deps::ProviderRouteDeps,
    ) -> SessionControlHandle {
        SessionControlHandle::new(SessionControlHandleParts {
            session_stores: self.session_store_lookup(),
            session_runtime: Arc::clone(&self.sessions),
            scheduler_spawner: SessionMessageSchedulerSpawner::new(Arc::downgrade(
                &self.scheduler_worker_host,
            )),
            perf_telemetry: self.perf_telemetry.clone(),
            provider_launch: provider_routes.provider_workspace_launch_runtime(),
            session_publication: self.session_publication_effects(),
            ask_user_question: Arc::clone(&self.ask_user_question),
            provider_unknown_events: self.provider_unknown_events.clone(),
        })
    }
    pub fn session_file_completions(&self) -> SessionFileCompletionsHandle {
        SessionFileCompletionsHandle::new(SessionFileCompletionsHandleParts {
            global_store: self.global_store.clone(),
            session_stores: self.session_store_lookup(),
            workspace_stores: self.workspace_store_lookup(),
            worktree_file_completions_cache: Arc::clone(&self.worktree_file_completions_cache),
            perf_telemetry: self.perf_telemetry.clone(),
            data_root: self.data_root.clone(),
            daemon_url: self.daemon_url.clone(),
            harness: Arc::clone(&self.harness),
        })
    }
    pub fn session_title_model_mode(&self) -> SessionTitleModelModeHandle {
        SessionTitleModelModeHandle::new(SessionTitleModelModeHandleParts {
            global_store: self.global_store.clone(),
            session_stores: self.session_store_lookup(),
            workspace_stores: self.workspace_store_lookup(),
            session_runtime: Arc::clone(&self.sessions),
            active_snapshot: Arc::clone(&self.active_snapshot),
            provider_runtime: Arc::clone(&self.providers),
            plugins: Arc::clone(&self.plugins),
            ops_events: self.ops_events.clone(),
            data_root: self.data_root.clone(),
            daemon_url: self.daemon_url.clone(),
            auth_token: self.auth_token.clone(),
            harness: Arc::clone(&self.harness),
        })
    }
    pub fn session_message_command(&self) -> SessionMessageCommandHandle {
        SessionMessageCommandHandle::new(
            self.global_store.clone(),
            self.session_store_lookup(),
            Arc::clone(&self.sessions),
            Arc::clone(&self.update_drain),
            self.data_root.clone(),
            self.session_title_model_mode(),
            SessionMessageSchedulerSpawner::new(Arc::downgrade(&self.scheduler_worker_host)),
        )
    }
    pub fn session_subagent_read(&self) -> SessionSubagentReadHandle {
        SessionSubagentReadHandle::new(self.session_store_lookup())
    }
    pub fn session_subagent_mcp_read(&self) -> SessionSubagentMcpReadHandle {
        let provider_inactivity_timeout = Arc::new({
            let sessions = Arc::clone(&self.sessions);
            move || {
                let sessions = Arc::clone(&sessions);
                Box::pin(async move { sessions.provider_inactivity_timeout().await })
                    as SessionSubagentMcpReadFuture<_>
            }
        });
        let emit_legacy_context_window_key_reject = Arc::new({
            let perf_telemetry = self.perf_telemetry.clone();
            move |legacy_key: String| {
                let perf_telemetry = perf_telemetry.clone();
                Box::pin(async move {
                    let mut labels = HashMap::new();
                    labels.insert("source".to_string(), "daemon".to_string());
                    labels.insert(
                        "surface".to_string(),
                        "sessions.context_window_summary".to_string(),
                    );
                    labels.insert("issue".to_string(), "legacy_context_window_key".to_string());
                    labels.insert("legacy_key".to_string(), legacy_key);
                    perf_telemetry
                        .record_metric(
                            PerfMetric {
                                name: "compat.payload_reject_count".to_string(),
                                kind: PerfMetricKind::Counter,
                                unit: "count".to_string(),
                                value: 1.0,
                                labels,
                            },
                            None,
                            None,
                            None,
                        )
                        .await;
                }) as SessionSubagentMcpReadFuture<_>
            }
        });
        SessionSubagentMcpReadHandle::new(
            self.session_store_lookup(),
            provider_inactivity_timeout,
            emit_legacy_context_window_key_reject,
        )
    }
    pub(super) fn session_subagent_mcp_control_with_provider_routes(
        &self,
        provider_routes: &provider_deps::ProviderRouteDeps,
    ) -> SessionSubagentMcpControlHandle {
        let provider_inactivity_timeout = Arc::new({
            let sessions = Arc::clone(&self.sessions);
            move || {
                let sessions = Arc::clone(&sessions);
                Box::pin(async move { sessions.provider_inactivity_timeout().await })
                    as SessionSubagentMcpControlFuture<_>
            }
        });
        let emit_legacy_context_window_key_reject = Arc::new({
            let perf_telemetry = self.perf_telemetry.clone();
            move |legacy_key: String| {
                let perf_telemetry = perf_telemetry.clone();
                Box::pin(async move {
                    let mut labels = HashMap::new();
                    labels.insert("source".to_string(), "daemon".to_string());
                    labels.insert(
                        "surface".to_string(),
                        "sessions.context_window_summary".to_string(),
                    );
                    labels.insert("issue".to_string(), "legacy_context_window_key".to_string());
                    labels.insert("legacy_key".to_string(), legacy_key);
                    perf_telemetry
                        .record_metric(
                            PerfMetric {
                                name: "compat.payload_reject_count".to_string(),
                                kind: PerfMetricKind::Counter,
                                unit: "count".to_string(),
                                value: 1.0,
                                labels,
                            },
                            None,
                            None,
                            None,
                        )
                        .await;
                }) as SessionSubagentMcpControlFuture<_>
            }
        });
        let session_stores = self.session_store_lookup();
        let scheduler_spawner = SessionSubagentMcpControlSchedulerSpawner::new(Arc::downgrade(
            &self.scheduler_worker_host,
        ));
        let publish_host = SessionSubagentMcpControlPublicationHost::new(
            session_stores.clone(),
            self.workspace_store_lookup(),
            Arc::clone(&self.active_snapshot),
        );
        let child_run_host = SubagentChildRunHost::new(
            self.weak_session_store_lookup(),
            SessionEventHeadSubscriber::new(Arc::downgrade(&self.sessions)),
            Arc::clone(&self.active_snapshot),
        );
        let worktree_host = self.task_worktree_host();
        let spawn_host = Arc::new(SubagentSpawnHost::new(SubagentSpawnHostParts {
            session_stores: session_stores.clone(),
            session_runtime: Arc::clone(&self.sessions),
            scheduler_spawner: scheduler_spawner.clone(),
            publish_host: publish_host.clone(),
            child_run_host,
            session_vcs: self.session_vcs(),
            worktrees: worktree_host,
            provider_launch: provider_routes.provider_workspace_launch_runtime(),
            global_store: self.global_store.clone(),
            perf_telemetry: self.perf_telemetry.clone(),
            data_root: self.data_root.clone(),
        }));
        let archive_worktree_cleanup = Arc::new(
            crate::daemon::sessions::subagents::SubagentArchiveWorktreeCleanupHost::new(
                self.data_root.clone(),
                self.global_store.clone(),
                crate::daemon::workspaces::vcs_hooks::WorkspaceVcsHookHost::new(
                    self.data_root.clone(),
                    self.daemon_url.clone(),
                    self.global_store.clone(),
                    self.workspace_store_lookup(),
                    Arc::clone(&self.harness),
                ),
            ),
        );
        SessionSubagentMcpControlHandle::new(SessionSubagentMcpControlHandleParts {
            session_stores,
            session_runtime: Arc::clone(&self.sessions),
            scheduler_spawner,
            publish_host,
            lifecycle_host: SessionSubagentMcpControlLifecycleHost::new(
                self.global_store.clone(),
                Arc::clone(&self.active_snapshot),
                Arc::clone(&self.providers),
            ),
            active_snapshot: Arc::clone(&self.active_snapshot),
            spawn_host,
            archive_worktree_cleanup,
            provider_inactivity_timeout,
            emit_legacy_context_window_key_reject,
        })
    }
    pub fn session_read_models(&self) -> SessionReadModelsHandle {
        SessionReadModelsHandle::new(
            self.global_store.clone(),
            self.session_store_lookup(),
            self.stores.clone(),
            Arc::clone(&self.active_snapshot),
            self.tool_output_spool_dir.clone(),
            self.perf_telemetry.clone(),
        )
    }
    fn session_artifact_effects(&self) -> Arc<SessionArtifactEffects> {
        self.session_publication_effects()
            .session_artifact_effects()
    }
    pub fn session_artifacts(&self) -> SessionArtifactsHandle {
        SessionArtifactsHandle::new(
            self.session_store_lookup(),
            self.tool_output_spool_dir.clone(),
            self.session_artifact_effects(),
        )
    }
    fn session_vcs_effects(&self) -> Arc<SessionVcsEffects> {
        let vcs_runtime = self.worktree_vcs_runtime.clone();
        let vcs_execution = self.worktree_vcs_execution.clone();
        let worktree_has_vcs_repo = Arc::new({
            let vcs_execution = vcs_execution.clone();
            move |worktree: Worktree| {
                let vcs_execution = vcs_execution.clone();
                Box::pin(async move {
                    crate::daemon::git_status::worktree_has_vcs_repo(&vcs_execution, &worktree)
                        .await
                }) as SessionVcsFuture<_>
            }
        });
        let load_git_status_snapshot = Arc::new({
            let vcs_execution = vcs_execution.clone();
            move |worktree: Worktree, include_untracked_files: bool, include_entries: bool| {
                let vcs_execution = vcs_execution.clone();
                Box::pin(async move {
                    crate::daemon::git_status::load_git_status_snapshot(
                        &vcs_execution,
                        &worktree,
                        include_untracked_files,
                        include_entries,
                    )
                    .await
                }) as SessionVcsFuture<_>
            }
        });
        let resolve_worktree_commit = Arc::new({
            let vcs_execution = vcs_execution.clone();
            move |worktree: Worktree, revision: String| {
                let vcs_execution = vcs_execution.clone();
                Box::pin(async move {
                    let source = crate::daemon::git_status::HttpWorktreeVcsSource::new(
                        &vcs_execution,
                        &worktree,
                    );
                    source.resolve_commit(&revision).await
                }) as SessionVcsFuture<_>
            }
        });
        let diff_worktree_for_session = Arc::new({
            let vcs_execution = vcs_execution.clone();
            move |worktree: Worktree, base_commit_sha: String| {
                let vcs_execution = vcs_execution.clone();
                Box::pin(async move {
                    crate::daemon::workspaces::diff_worktree_for_session(
                        &vcs_execution,
                        &worktree,
                        &base_commit_sha,
                    )
                    .await
                }) as SessionVcsFuture<_>
            }
        });
        let diff_worktree_summary_for_session = Arc::new({
            let vcs_execution = vcs_execution.clone();
            move |worktree: Worktree, base_commit_sha: String| {
                let vcs_execution = vcs_execution.clone();
                Box::pin(async move {
                    crate::daemon::workspaces::diff_worktree_summary_for_session(
                        &vcs_execution,
                        &worktree,
                        &base_commit_sha,
                    )
                    .await
                }) as SessionVcsFuture<_>
            }
        });
        let resolve_worktree_diff_base = Arc::new({
            let vcs_execution = vcs_execution.clone();
            move |worktree: Worktree, query: SessionVcsDiffBaseQuery| {
                let vcs_execution = vcs_execution.clone();
                Box::pin(async move {
                    let source = crate::daemon::git_status::HttpWorktreeVcsSource::new(
                        &vcs_execution,
                        &worktree,
                    );
                    ctx_worktree_vcs_service::resolve_worktree_diff_base_from_source(
                        &source,
                        &worktree,
                        WorktreeVcsDiffBaseQuery {
                            base_commit_sha: query.base_commit_sha,
                            target_branch: query.target_branch,
                        },
                    )
                    .await
                }) as SessionVcsFuture<_>
            }
        });
        let apply_worktree_vcs_session_patch =
            Arc::new(|worktree: Worktree, patch: String, reverse_patch: bool| {
                Box::pin(async move {
                    ctx_worktree_vcs_service::apply_worktree_vcs_session_patch(
                        Path::new(&worktree.root_path),
                        &patch,
                        reverse_patch,
                    )
                    .await
                }) as SessionVcsFuture<_>
            });
        let cached_worktree_vcs_snapshot = Arc::new({
            let vcs_runtime = vcs_runtime.clone();
            let vcs_execution = vcs_execution.clone();
            move |worktree_id: WorktreeId| {
                let vcs_runtime = vcs_runtime.clone();
                let vcs_execution = vcs_execution.clone();
                Box::pin(async move {
                    vcs_runtime
                        .get_worktree_vcs_snapshot(&vcs_execution, worktree_id)
                        .await
                }) as SessionVcsFuture<_>
            }
        });
        let emit_compat_payload_reject_counter = Arc::new({
            let perf_telemetry = self.perf_telemetry.clone();
            move |surface: &'static str, issue: &'static str| {
                let perf_telemetry = perf_telemetry.clone();
                Box::pin(async move {
                    let mut labels = HashMap::new();
                    labels.insert("source".to_string(), "daemon".to_string());
                    labels.insert("surface".to_string(), surface.to_string());
                    labels.insert("issue".to_string(), issue.to_string());
                    let metric = PerfMetric {
                        name: "compat.payload_reject_count".to_string(),
                        kind: PerfMetricKind::Counter,
                        unit: "count".to_string(),
                        value: 1.0,
                        labels,
                    };
                    perf_telemetry.record_metric(metric, None, None, None).await;
                }) as SessionVcsFuture<_>
            }
        });
        SessionVcsEffects::new(SessionVcsEffectsParts {
            worktree_has_vcs_repo,
            load_git_status_snapshot,
            resolve_worktree_commit,
            diff_worktree_for_session,
            diff_worktree_summary_for_session,
            resolve_worktree_diff_base,
            apply_worktree_vcs_session_patch,
            cached_worktree_vcs_snapshot,
            emit_compat_payload_reject_counter,
            is_no_vcs_repo_error: Arc::new(ctx_worktree_vcs_service::is_no_vcs_repo_error),
        })
    }
    pub fn session_vcs(&self) -> SessionVcsHandle {
        SessionVcsHandle::new(self.session_store_lookup(), self.session_vcs_effects())
    }
}
