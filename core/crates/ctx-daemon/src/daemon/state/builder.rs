mod runtime_parts;
#[cfg(not(test))]
mod startup;

use super::*;
use crate::daemon::provider_capability_hosts::{
    ProviderLifecycleBackgroundHost, ProviderLifecycleBackgroundHostParts,
};
use crate::daemon::scheduler::SessionSchedulerWorkerHostParts;
use crate::daemon::sessions::SessionSchedulerWorkerHostFactory;
use crate::daemon::task_session_effects::{
    SessionPublicationEffects, TaskPublicationHost, TaskSessionCleanupHost,
};
use crate::daemon::ProviderWorkspaceLaunchRuntime;
use ctx_storage_admission::StorageGuardRuntime;
use ctx_worktree_vcs_service::worktree_vcs_enabled_from_env;
use runtime_parts::{
    build_execution_runtime, build_provider_runtime, build_runtime_parts, build_telemetry_runtime,
    build_tool_output_spool, build_transport_runtime, build_workspace_runtime,
};

impl DaemonState {
    pub fn new(
        data_root: PathBuf,
        stores: StoreManager,
        providers: HashMap<String, Arc<dyn ProviderAdapter>>,
        daemon_url: String,
        auth_token: Option<String>,
    ) -> Self {
        Self::new_with_public_base_url(data_root, stores, providers, daemon_url, None, auth_token)
    }

    pub fn new_with_public_base_url(
        data_root: PathBuf,
        stores: StoreManager,
        providers: HashMap<String, Arc<dyn ProviderAdapter>>,
        daemon_url: String,
        public_base_url: Option<String>,
        auth_token: Option<String>,
    ) -> Self {
        Self::new_with_runtime_flags(
            data_root,
            stores,
            providers,
            daemon_url,
            public_base_url,
            auth_token,
            AppRuntimeFlags {
                worktree_vcs_enabled: worktree_vcs_enabled_from_env(),
            },
        )
    }

    pub fn new_with_runtime_flags(
        data_root: PathBuf,
        stores: StoreManager,
        providers: HashMap<String, Arc<dyn ProviderAdapter>>,
        daemon_url: String,
        public_base_url: Option<String>,
        auth_token: Option<String>,
        runtime_flags: AppRuntimeFlags,
    ) -> Self {
        let worktree_vcs_enabled = runtime_flags.worktree_vcs_enabled;
        let tool_output_spool = build_tool_output_spool(&data_root);
        let runtime_parts = build_runtime_parts(&data_root);
        let storage_guard = Arc::new(StorageGuardRuntime::new(&data_root));
        let local_shutdown_token = std::env::var("CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN")
            .ok()
            .filter(|value| !value.trim().is_empty());
        #[cfg(not(test))]
        startup::spawn_startup_prewarm_loader(
            data_root.clone(),
            Arc::clone(&runtime_parts.execution_setup),
        );

        let mcp_auth = Arc::new(ctx_mcp_auth::McpAuthRegistry::new());
        let update_drain = Arc::new(ctx_update_service::UpdateDrainCoordinator::new());
        let sessions = Arc::new(SessionRuntime::new_from_env());
        let workspaces = build_workspace_runtime(
            worktree_vcs_enabled,
            runtime_parts.workspace_active_snapshot,
        );
        let providers = build_provider_runtime(providers);
        let telemetry = build_telemetry_runtime(
            runtime_parts.telemetry,
            runtime_parts.ops_events,
            runtime_parts.perf_telemetry,
            runtime_parts.provider_unknown_events,
        );
        let transport =
            build_transport_runtime(runtime_parts.terminals, runtime_parts.web_sessions);
        let execution =
            build_execution_runtime(runtime_parts.harness_runtime, runtime_parts.execution_setup);
        let workspace_stores = ProtectedWorkspaceStoreLookup::new(
            stores.clone(),
            Arc::clone(&sessions),
            Arc::clone(&transport.merge_queue),
        );
        let session_stores =
            SessionStoreLookup::new(stores.global().clone(), workspace_stores.clone());
        let task_publication = Arc::new(TaskPublicationHost::new(
            workspace_stores.clone(),
            Arc::clone(&workspaces.workspace_active_snapshot),
        ));
        let session_publication = SessionPublicationEffects::new(
            Arc::clone(&sessions),
            session_stores.clone(),
            Arc::clone(&task_publication),
        );
        let provider_lifecycle_background = Arc::new(ProviderLifecycleBackgroundHost::new(
            ProviderLifecycleBackgroundHostParts {
                data_root: data_root.clone(),
                providers: Arc::clone(&providers),
                resource_sampler: Arc::clone(&telemetry.resource_sampler),
                sessions: Arc::clone(&sessions),
                session_stores: session_stores.clone(),
                session_publication: session_publication.clone(),
                perf_telemetry: telemetry.perf_telemetry.clone(),
                shutdown_tx: runtime_parts.shutdown_tx.clone(),
            },
        ));
        let task_session_cleanup = TaskSessionCleanupHost::new(
            stores.global().clone(),
            Arc::clone(&sessions),
            Arc::clone(&providers),
            Arc::clone(&workspaces.workspace_active_snapshot),
            workspace_stores.clone(),
        );
        let provider_launch_runtime = Arc::new(ProviderWorkspaceLaunchRuntime::new(
            data_root.clone(),
            daemon_url.clone(),
            auth_token.clone(),
            workspace_stores.clone(),
            Arc::clone(&providers),
            telemetry.ops_events.clone(),
            Arc::clone(&execution.harness),
        ));
        let session_scheduler_worker_host =
            SessionSchedulerWorkerHostFactory::new(SessionSchedulerWorkerHostParts {
                session_stores,
                session_runtime: Arc::clone(&sessions),
                workspace_stores,
                active_snapshot: Arc::clone(&workspaces.workspace_active_snapshot),
                global_store: stores.global().clone(),
                providers: Arc::clone(&providers),
                provider_launch_runtime,
                worktree_bootstrap_gates: Arc::clone(&workspaces.worktree_bootstrap_gates),
                storage_guard: Arc::clone(&storage_guard),
                update_drain: Arc::clone(&update_drain),
                mcp_auth: Arc::clone(&mcp_auth),
                perf_telemetry: telemetry.perf_telemetry.clone(),
                telemetry: telemetry.telemetry.clone(),
                provider_unknown_events: telemetry.provider_unknown_events.clone(),
                resource_sampler: Arc::clone(&telemetry.resource_sampler),
                tool_output_spool_enabled: tool_output_spool.enabled,
                tool_output_spool_dir: tool_output_spool.dir.clone(),
                ops_events: telemetry.ops_events.clone(),
            });

        Self {
            core: CoreState {
                data_root,
                storage_guard,
                tool_output_spool_dir: tool_output_spool.dir,
                stores,
                daemon_url,
                public_base_url,
                auth_token,
                local_shutdown_token,
                mcp_auth,
                ask_user_question: runtime_parts.ask_user_question,
                shutdown_tx: runtime_parts.shutdown_tx,
                update_drain,
            },
            sessions,
            workspaces,
            providers,
            telemetry,
            transport,
            execution,
            session_publication,
            provider_lifecycle_background,
            task_publication,
            task_session_cleanup,
            session_scheduler_worker_host,
        }
    }
}
