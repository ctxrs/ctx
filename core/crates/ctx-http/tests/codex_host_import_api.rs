use axum::http::StatusCode;
use ctx_provider_accounts::{codex_env_for_active_account, ensure_codex_auth_ready};
use serde::Deserialize;
use serde_json::json;

mod common;

#[derive(Debug, Deserialize)]
struct CodexEndpointProfile {
    api_shape: String,
    auth_type: String,
}

#[derive(Debug, Deserialize)]
struct CodexAccountEntry {
    id: String,
    label: String,
    kind: String,
    endpoint_profile: CodexEndpointProfile,
}

#[derive(Debug, Deserialize)]
struct CodexAccountsResponse {
    active_account_id: Option<String>,
    accounts: Vec<CodexAccountEntry>,
}

#[derive(Debug, Deserialize)]
struct CodexHostImportProbe {
    available: bool,
    auth_kind: Option<String>,
}

#[tokio::test]
async fn host_import_probe_and_import_projects_runtime_auth() {
    let _env_lock = common::process_env_test_lock().lock().await;
    let host_dir = tempfile::tempdir().unwrap();
    let host_auth_path = host_dir.path().join("auth.json");
    tokio::fs::write(&host_auth_path, br#"{"OPENAI_API_KEY":"test-key"}"#)
        .await
        .unwrap();

    let _host_auth_path = common::TestEnvGuard::set(
        "CTX_CODEX_HOST_AUTH_PATH",
        host_auth_path.to_string_lossy().as_ref(),
    );
    let _codex_home = common::TestEnvGuard::unset("CTX_CODEX_HOME");

    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await;
    let base = &server.base_url;
    let client = &server.client;

    let probe = client
        .get(format!("{base}/api/providers/codex/import/host"))
        .send()
        .await
        .unwrap();
    assert_eq!(probe.status(), StatusCode::OK);
    let probe_body: CodexHostImportProbe = probe.json().await.unwrap();
    assert!(probe_body.available);
    assert_eq!(probe_body.auth_kind.as_deref(), Some("api_key"));

    let imported = client
        .post(format!("{base}/api/providers/codex/import/host"))
        .json(&json!({ "label": "Imported Host Auth" }))
        .send()
        .await
        .unwrap();
    assert_eq!(imported.status(), StatusCode::OK);
    let imported_body: CodexAccountsResponse = imported.json().await.unwrap();
    assert_eq!(imported_body.accounts.len(), 1);
    let active_id = imported_body.active_account_id.expect("active account");
    let account = imported_body
        .accounts
        .iter()
        .find(|entry| entry.id == active_id)
        .expect("active account entry");
    assert_eq!(account.label, "Imported Host Auth");
    assert_eq!(account.kind, "api_key");
    assert_eq!(account.endpoint_profile.api_shape, "openai_responses");
    assert_eq!(account.endpoint_profile.auth_type, "bearer");

    let env = codex_env_for_active_account(fixture.data_dir.path())
        .await
        .unwrap();
    let home = env.get("CODEX_HOME").expect("CODEX_HOME");
    ensure_codex_auth_ready(std::path::Path::new(home))
        .await
        .unwrap();
}
