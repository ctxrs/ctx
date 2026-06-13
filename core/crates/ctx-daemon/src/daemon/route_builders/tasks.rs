use super::*;

impl task_deps::TaskRouteDeps {
    fn task_lifecycle_effects(
        &self,
        session_routes: &session_deps::SessionRouteDeps,
    ) -> Arc<TaskLifecycleEffects> {
        crate::daemon::task_session_effects::task_lifecycle_effects(
            session_routes.task_publication_host(),
            session_routes.task_session_cleanup_host(),
        )
    }
    pub(super) fn task_lifecycle_with_session_routes(
        &self,
        session_routes: &session_deps::SessionRouteDeps,
    ) -> TaskLifecycleHandle {
        TaskLifecycleHandle::new(
            self.global_store.clone(),
            session_routes.workspace_store_lookup(),
            session_routes.task_worktree_host(),
            self.task_lifecycle_effects(session_routes),
        )
    }
    fn task_store_lookup(&self) -> TaskStoreLookup {
        TaskStoreLookup::new(self.global_store.clone(), self.workspace_store_lookup())
    }
    fn task_metadata_effects(
        &self,
        session_routes: &session_deps::SessionRouteDeps,
    ) -> Arc<TaskMetadataEffects> {
        crate::daemon::task_session_effects::task_metadata_effects(
            session_routes.task_publication_host(),
        )
    }
    pub fn task_listing(&self) -> TaskListingHandle {
        let snapshot = Arc::clone(&self.active_snapshot);
        let archived_rev_loader: TaskArchivedRevLoader = Arc::new(move |workspace_id| {
            let snapshot = Arc::clone(&snapshot);
            Box::pin(async move {
                let (_, archived_rev) = snapshot.snapshot_state(workspace_id).await;
                archived_rev
            })
        });
        TaskListingHandle::new(self.workspace_store_lookup(), archived_rev_loader)
    }
    pub fn task_session_listing(&self) -> TaskSessionListingHandle {
        TaskSessionListingHandle::new(self.task_store_lookup())
    }
    pub(super) fn task_read_state_with_session_routes(
        &self,
        session_routes: &session_deps::SessionRouteDeps,
    ) -> TaskReadStateHandle {
        TaskReadStateHandle::new(
            self.task_store_lookup(),
            self.task_metadata_effects(session_routes),
        )
    }
    pub(super) fn task_title_with_session_routes(
        &self,
        session_routes: &session_deps::SessionRouteDeps,
    ) -> TaskTitleHandle {
        let web_sessions = Arc::clone(&self.web_sessions);
        let close_web_sessions_for_task: TaskCloseWebSessionsForTask =
            Arc::new(move |session_ids, worktree_ids| {
                let web_sessions = Arc::clone(&web_sessions);
                Box::pin(async move {
                    web_sessions
                        .close_for_task(&session_ids, &worktree_ids)
                        .await
                })
            });
        TaskTitleHandle::new(
            self.task_store_lookup(),
            self.task_metadata_effects(session_routes),
            close_web_sessions_for_task,
        )
    }
    fn task_session_admission_effects_with_title_mode(
        &self,
        session_routes: &session_deps::SessionRouteDeps,
        session_title_model_mode: SessionTitleModelModeHandle,
    ) -> Arc<TaskAdmissionSessionEffects> {
        crate::daemon::task_session_effects::task_admission_session_effects(
            session_routes.session_publication_effects(),
            Arc::clone(&self.sessions),
            SessionMessageSchedulerSpawner::new(Arc::downgrade(&self.scheduler_worker_host)),
            session_title_model_mode,
            session_routes.task_publication_host(),
        )
    }
    fn task_session_admission_provider_status(&self) -> ProviderStatusHandle {
        ProviderStatusHandle::new(
            self.data_root.clone(),
            Arc::clone(&self.providers),
            self.ops_events.clone(),
        )
    }
    fn task_session_admission_model_catalog_loader_with_provider_routes(
        &self,
        provider_routes: &provider_deps::ProviderRouteDeps,
    ) -> TaskAdmissionModelCatalogLoader {
        let launch = provider_routes.provider_workspace_launch_runtime();
        Arc::new(
            move |workspace: Workspace,
                  provider_id: String,
                  execution_environment: ExecutionEnvironment| {
                let launch = Arc::clone(&launch);
                Box::pin(async move {
                    crate::daemon::sessions::model_catalog::load_provider_model_catalog_for_execution_environment(
                        launch.as_ref(),
                        &workspace,
                        &provider_id,
                        execution_environment,
                    )
                    .await
                }) as TaskAdmissionFuture<_>
            },
        )
    }
    pub(super) fn task_session_admission_with_route_deps(
        &self,
        provider_routes: &provider_deps::ProviderRouteDeps,
        session_routes: &session_deps::SessionRouteDeps,
        session_title_model_mode: SessionTitleModelModeHandle,
    ) -> TaskSessionAdmissionHandle {
        TaskSessionAdmissionHandle::new(
            self.global_store.clone(),
            session_routes.workspace_store_lookup(),
            Arc::clone(&self.sessions),
            Arc::clone(&self.providers),
            self.task_session_admission_provider_status(),
            session_routes.task_worktree_host(),
            self.task_session_admission_effects_with_title_mode(
                session_routes,
                session_title_model_mode,
            ),
            self.task_session_admission_model_catalog_loader_with_provider_routes(provider_routes),
            self.telemetry.clone(),
            self.ops_events.clone(),
            self.perf_telemetry.clone(),
        )
    }
    pub(super) fn task_creation_with_session_admission(
        &self,
        task_session_admission: TaskSessionAdmissionHandle,
        session_routes: &session_deps::SessionRouteDeps,
    ) -> TaskCreationHandle {
        TaskCreationHandle::new(
            self.global_store.clone(),
            session_routes.workspace_store_lookup(),
            task_session_admission,
            self.task_lifecycle_with_session_routes(session_routes),
        )
    }
}
