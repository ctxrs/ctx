use super::*;

pub(super) struct ModelCatalogFixture {
    host: crate::daemon::ProviderWorkspaceLaunchRuntime,
    workspace: Workspace,
}

impl ModelCatalogFixture {
    pub(super) fn host(&self) -> &crate::daemon::ProviderWorkspaceLaunchRuntime {
        &self.host
    }

    pub(super) fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    pub(super) fn global_store(&self) -> &Store {
        self.host.global_store()
    }

    pub(super) fn providers(&self) -> &ctx_provider_runtime::ProviderRuntime {
        self.host.providers()
    }
}

pub(super) async fn test_model_catalog_fixture(data_root: &Path, port: u16) -> ModelCatalogFixture {
    let stores = StoreManager::open(data_root).await.expect("open stores");
    let sessions = Arc::new(ctx_session_runtime::runtime::SessionRuntime::<
        crate::daemon::scheduler::SchedulerCommand,
    >::new_from_env());
    let workspace_stores = crate::daemon::ProtectedWorkspaceStoreLookup::new(
        stores.clone(),
        sessions,
        Arc::new(ctx_merge_queue::MergeQueueRuntime::new()),
    );
    let providers = Arc::new(ctx_provider_runtime::ProviderRuntime::new(HashMap::new()));
    let host = crate::daemon::ProviderWorkspaceLaunchRuntime::new(
        data_root.to_path_buf(),
        format!("http://127.0.0.1:{port}"),
        None,
        workspace_stores,
        providers,
        ctx_observability::ops_events::OpsEvents::new(data_root.to_path_buf()),
        Arc::new(ctx_workspace_runtime::HarnessRuntimeManager::new(
            data_root.to_path_buf(),
        )),
    );
    let workspace = host
        .global_store()
        .create_workspace(
            "ws".to_string(),
            data_root.join("repo").to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .expect("create workspace");
    ModelCatalogFixture { host, workspace }
}

pub(super) async fn save_sandbox_execution_mode(fixture: &ModelCatalogFixture) {
    ctx_settings_service::save_settings(
        fixture.global_store(),
        &Settings {
            execution: Some(ExecutionSettings {
                mode: ExecutionMode::Sandbox,
                ..ExecutionSettings::default()
            }),
            ..Settings::default()
        },
    )
    .await
    .expect("save settings");
}

pub(super) async fn seed_ready_gemini_status(fixture: &ModelCatalogFixture) {
    fixture
        .providers()
        .upsert_provider_status(
            "gemini".to_string(),
            ctx_providers::adapters::ProviderStatus {
                provider_id: "gemini".to_string(),
                installed: true,
                detected_path: None,
                version: Some("0.33.1".to_string()),
                capabilities: None,
                health: ctx_providers::adapters::ProviderHealth::Ok,
                diagnostics: Vec::new(),
                details: HashMap::new(),
                usability: ctx_providers::adapters::ProviderUsability::default(),
            },
        )
        .await;
}

pub(super) fn write_invalid_harness_registry(data_root: &Path) {
    let path = data_root
        .join("providers")
        .join("harness_sources")
        .join("registry.json");
    std::fs::create_dir_all(path.parent().expect("registry parent")).expect("mkdir registry");
    std::fs::write(path, "{ not valid json").expect("write invalid registry");
}

pub(super) fn write_invalid_agent_server_config(data_root: &Path) {
    let path = data_root
        .join("providers")
        .join("agent-servers")
        .join("agent_servers.json");
    std::fs::create_dir_all(path.parent().expect("agent server config parent"))
        .expect("mkdir agent server config");
    std::fs::write(path, "{ not valid json").expect("write invalid agent server config");
}
