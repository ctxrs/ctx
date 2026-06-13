use super::*;
use chrono::Utc;
use ctx_core::env::DAEMON_AUTH_ENV_VARS;
use ctx_core::ids::WorkspaceId;
use ctx_core::provider_policy::CTX_CRP_LAUNCH_POLICY_FULL;
use ctx_harness_sources::{
    mark_endpoint_verification, set_provider_source_selection, upsert_provider_endpoint,
    HarnessApiShape, HarnessEndpointUpsert, HarnessEndpointVerificationStatus, HarnessRouteBackend,
    HarnessRuntimeSourceMode, HarnessSourceKind,
};
use std::sync::Arc;

#[derive(Clone)]
struct TestProbeHost {
    data_root: PathBuf,
    daemon_url: String,
    auth_token: Option<String>,
    workspace: Arc<Workspace>,
    runtime: PreparedWorkspaceProbeRuntime,
}

#[async_trait]
impl ProviderProbeHost for TestProbeHost {
    fn data_root(&self) -> &Path {
        &self.data_root
    }

    fn daemon_url(&self) -> &str {
        &self.daemon_url
    }

    fn auth_token(&self) -> Option<&String> {
        self.auth_token.as_ref()
    }

    fn redact_sensitive(&self, input: &str) -> String {
        input.to_string()
    }

    async fn load_workspace(&self, workspace_id: WorkspaceId) -> Result<Option<Workspace>, String> {
        if self.workspace.id == workspace_id {
            Ok(Some((*self.workspace).clone()))
        } else {
            Ok(None)
        }
    }

    async fn prepare_workspace_probe_runtime(
        &self,
        _workspace: &Workspace,
    ) -> Result<PreparedWorkspaceProbeRuntime, String> {
        Ok(self.runtime.clone())
    }

    async fn prepare_worktree_probe_runtime(
        &self,
        _workspace: &Workspace,
        _worktree: &Worktree,
    ) -> Result<PreparedWorkspaceProbeRuntime, String> {
        Ok(self.runtime.clone())
    }
}

#[tokio::test]
async fn codex_endpoint_probe_runtime_preserves_openrouter_base_url_and_model_provider() {
    let root = tempfile::tempdir().expect("tempdir");
    let data_root = root.path().join("data-root");
    let runtime_root = root.path().join("runtime-root");
    let workspace_root = root.path().join("workspace");
    tokio::fs::create_dir_all(&data_root)
        .await
        .expect("create data root");
    tokio::fs::create_dir_all(&runtime_root)
        .await
        .expect("create runtime root");
    tokio::fs::create_dir_all(&workspace_root)
        .await
        .expect("create workspace root");

    let endpoint = upsert_provider_endpoint(
        &data_root,
        "codex",
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "OpenRouter".to_string(),
            base_url: Some("https://openrouter.ai/api/v1".to_string()),
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: None,
            model_override: Some("openai/gpt-4.1-mini".to_string()),
            api_key: Some("sk-or-test".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert endpoint");
    set_provider_source_selection(
        &data_root,
        "codex",
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select endpoint");
    mark_endpoint_verification(
        &data_root,
        "codex",
        &endpoint.id,
        HarnessEndpointVerificationStatus::Valid,
        None,
    )
    .await
    .expect("mark verified");

    let workspace = Arc::new(Workspace {
        id: WorkspaceId::new(),
        name: "ws".to_string(),
        root_path: workspace_root.to_string_lossy().to_string(),
        created_at: Utc::now(),
        vcs_kind: None,
    });
    let host = TestProbeHost {
        data_root: data_root.clone(),
        daemon_url: "http://127.0.0.1:0".to_string(),
        auth_token: Some("daemon-auth-token".to_string()),
        workspace: workspace.clone(),
        runtime: PreparedWorkspaceProbeRuntime {
            cwd: workspace_root.clone(),
            runtime_data_root: Some(runtime_root.clone()),
            env_overrides: HashMap::from([
                (
                    "CTX_DATA_ROOT".to_string(),
                    runtime_root.to_string_lossy().to_string(),
                ),
                (
                    CTX_CRP_LAUNCH_POLICY_ENV.to_string(),
                    CTX_CRP_LAUNCH_POLICY_FULL.to_string(),
                ),
            ]),
        },
    };

    let context = provider_auth_context_for_workspace_runtime(&host, &workspace, "codex")
        .await
        .expect("probe context");

    assert_eq!(context.source.source_kind, HarnessSourceKind::Endpoint);
    assert_eq!(
        context.source.runtime_source_mode(),
        HarnessRuntimeSourceMode::Endpoint(HarnessRouteBackend::UserManaged)
    );
    assert_eq!(
        context.env.get("CTX_MODEL_PROVIDER").map(String::as_str),
        Some("openrouter")
    );
    assert_eq!(
        context.env.get("OPENAI_BASE_URL").map(String::as_str),
        Some("https://openrouter.ai/api/v1")
    );
    assert!(
        !context.env.contains_key(CTX_CRP_LAUNCH_POLICY_ENV),
        "probe runtime env must not be able to spoof daemon-owned CRP launch policy"
    );

    let codex_home = PathBuf::from(
        context
            .env
            .get("CODEX_HOME")
            .cloned()
            .expect("runtime CODEX_HOME"),
    );
    assert!(
        codex_home.starts_with(&runtime_root),
        "expected runtime CODEX_HOME under runtime root, got {}",
        codex_home.display()
    );
    let auth = tokio::fs::read_to_string(codex_home.join("auth.json"))
        .await
        .expect("runtime auth json");
    assert!(auth.contains("\"OPENAI_BASE_URL\": \"https://openrouter.ai/api/v1\""));
}

#[tokio::test]
async fn provider_probe_env_omits_daemon_auth_token() {
    let root = tempfile::tempdir().expect("tempdir");
    let data_root = root.path().join("data-root");
    let workspace_root = root.path().join("workspace");
    tokio::fs::create_dir_all(&data_root)
        .await
        .expect("create data root");
    tokio::fs::create_dir_all(&workspace_root)
        .await
        .expect("create workspace root");

    let endpoint = upsert_provider_endpoint(
        &data_root,
        "codex",
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "OpenAI".to_string(),
            base_url: Some("https://api.openai.com/v1".to_string()),
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: None,
            model_override: Some("gpt-5.4".to_string()),
            api_key: Some("sk-test".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert endpoint");
    set_provider_source_selection(
        &data_root,
        "codex",
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select endpoint");

    let workspace = Arc::new(Workspace {
        id: WorkspaceId::new(),
        name: "ws".to_string(),
        root_path: workspace_root.to_string_lossy().to_string(),
        created_at: Utc::now(),
        vcs_kind: None,
    });
    let host = TestProbeHost {
        data_root: data_root.clone(),
        daemon_url: "http://127.0.0.1:4399".to_string(),
        auth_token: Some("daemon-auth-token".to_string()),
        workspace,
        runtime: PreparedWorkspaceProbeRuntime {
            cwd: workspace_root,
            runtime_data_root: None,
            env_overrides: HashMap::new(),
        },
    };

    let (_, env) = provider_probe_env(&host, "codex")
        .await
        .expect("provider probe env");

    assert_eq!(
        env.get("CTX_DAEMON_URL").map(String::as_str),
        Some("http://127.0.0.1:4399")
    );
    assert!(!env.contains_key("CTX_AUTH_TOKEN"));
    assert_eq!(env.get("CTX_MCP_DISABLED").map(String::as_str), Some("1"));
}

#[test]
fn probe_source_env_drops_daemon_owned_values() {
    let mut env = HashMap::new();

    insert_probe_source_env(&mut env, "OPENAI_BASE_URL", "https://example.com/v1");
    insert_probe_source_env(
        &mut env,
        CTX_CRP_LAUNCH_POLICY_ENV,
        CTX_CRP_LAUNCH_POLICY_FULL,
    );
    for key in DAEMON_AUTH_ENV_VARS {
        env.insert((*key).to_string(), "daemon-secret".to_string());
    }
    strip_daemon_owned_probe_env(&mut env);

    assert_eq!(
        env.get("OPENAI_BASE_URL").map(String::as_str),
        Some("https://example.com/v1")
    );
    assert!(!env.contains_key(CTX_CRP_LAUNCH_POLICY_ENV));
    for key in DAEMON_AUTH_ENV_VARS {
        assert!(!env.contains_key(*key), "{key} must not reach probe env");
    }
}

#[tokio::test]
async fn codex_ctx_managed_endpoint_probe_runtime_marks_ctx_managed_source_mode() {
    let root = tempfile::tempdir().expect("tempdir");
    let data_root = root.path().join("data-root");
    let runtime_root = root.path().join("runtime-root");
    let workspace_root = root.path().join("workspace");
    tokio::fs::create_dir_all(&data_root)
        .await
        .expect("create data root");
    tokio::fs::create_dir_all(&runtime_root)
        .await
        .expect("create runtime root");
    tokio::fs::create_dir_all(&workspace_root)
        .await
        .expect("create workspace root");

    let endpoint = upsert_provider_endpoint(
        &data_root,
        "codex",
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "ctx relay".to_string(),
            base_url: Some("https://api.ctx.rs/relay/openai/v1".to_string()),
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: None,
            model_override: Some("gpt-5.4".to_string()),
            api_key: Some("relay-test-token".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert endpoint");
    set_provider_source_selection(
        &data_root,
        "codex",
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select endpoint");
    mark_endpoint_verification(
        &data_root,
        "codex",
        &endpoint.id,
        HarnessEndpointVerificationStatus::Valid,
        None,
    )
    .await
    .expect("mark verified");

    let workspace = Arc::new(Workspace {
        id: WorkspaceId::new(),
        name: "ws".to_string(),
        root_path: workspace_root.to_string_lossy().to_string(),
        created_at: Utc::now(),
        vcs_kind: None,
    });
    let host = TestProbeHost {
        data_root: data_root.clone(),
        daemon_url: "http://127.0.0.1:0".to_string(),
        auth_token: Some("daemon-auth-token".to_string()),
        workspace: workspace.clone(),
        runtime: PreparedWorkspaceProbeRuntime {
            cwd: workspace_root.clone(),
            runtime_data_root: Some(runtime_root.clone()),
            env_overrides: HashMap::from([(
                "CTX_DATA_ROOT".to_string(),
                runtime_root.to_string_lossy().to_string(),
            )]),
        },
    };

    let context = provider_auth_context_for_workspace_runtime(&host, &workspace, "codex")
        .await
        .expect("probe context");

    assert_eq!(context.source.source_kind, HarnessSourceKind::Endpoint);
    assert_eq!(
        context.source.runtime_source_mode(),
        HarnessRuntimeSourceMode::Endpoint(HarnessRouteBackend::CtxManagedRelay)
    );
    assert_eq!(
        context
            .source
            .endpoint
            .as_ref()
            .map(|endpoint| endpoint.provider_id.as_str()),
        Some("codex")
    );
    assert_eq!(
        context
            .env
            .get(ctx_harness_sources::CTX_PROVIDER_ROUTE_BACKEND_ENV)
            .map(String::as_str),
        Some("ctx_managed")
    );
    assert_eq!(context.env.get("OPENAI_API_KEY").map(String::as_str), None);
    assert_eq!(context.env.get("OPENAI_BASE_URL").map(String::as_str), None);
    assert_eq!(context.env.get("CODEX_HOME").map(String::as_str), None);
    assert_eq!(
        context
            .env
            .get(ctx_harness_sources::CTX_LLM_RELAY_BASE_URL_ENV)
            .map(String::as_str),
        Some("https://api.ctx.rs/relay/openai/v1")
    );
}
