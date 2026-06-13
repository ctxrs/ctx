mod common;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use axum::http::StatusCode;
use ctx_daemon::test_support::TestDaemon;
use ctx_managed_installs::{save_agent_server_config, AgentServerCommand, AgentServerConfigFile};
use ctx_provider_accounts::add_gemini_account;
use ctx_providers::adapters::{ProviderHealth, ProviderStatus};

fn trimmed_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn required_absolute_existing_path(name: &str, label: &str) -> Option<String> {
    let raw = trimmed_env(name)?;
    let path = PathBuf::from(&raw);
    assert!(
        path.is_absolute() && path.exists(),
        "{label} must be an existing absolute path via {name}: {raw}"
    );
    Some(path.to_string_lossy().to_string())
}

fn resolve_bridge_path() -> Option<String> {
    if let Some(path) =
        required_absolute_existing_path("CTX_LIVE_ACP_CRP_BRIDGE_PATH", "live ACP bridge path")
    {
        return Some(path);
    }

    let output = Command::new("which").arg("acp-crp-bridge").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let detected = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    let path = PathBuf::from(&detected);
    if !path.is_absolute() || !path.exists() {
        return None;
    }
    Some(path.to_string_lossy().to_string())
}

fn normalize_version(version: &str) -> String {
    version
        .trim()
        .trim_start_matches('v')
        .trim_end_matches('.')
        .to_string()
}

fn managed_gemini_version() -> String {
    let matrix: serde_json::Value =
        serde_json::from_str(ctx_provider_accounts::PROVIDER_MATRIX_JSON).expect("provider matrix");
    matrix
        .get("providers")
        .and_then(serde_json::Value::as_array)
        .and_then(|providers| {
            providers.iter().find(|provider| {
                provider.get("id").and_then(serde_json::Value::as_str) == Some("gemini")
            })
        })
        .and_then(|provider| provider.get("releases"))
        .and_then(serde_json::Value::as_array)
        .and_then(|releases| releases.first())
        .and_then(|release| release.get("version"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .expect("managed gemini version")
}

fn live_gemini_cli_version(node_path: &str, cli_entry_path: &str) -> String {
    let output = Command::new(node_path)
        .arg(cli_entry_path)
        .arg("--version")
        .output()
        .expect("run live gemini --version");
    assert!(
        output.status.success(),
        "live Gemini CLI --version failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    normalize_version(&String::from_utf8_lossy(&output.stdout))
}

fn catalog_snapshot(models: &serde_json::Value) -> (String, Vec<String>) {
    let current_model_id = models
        .get("current_model_id")
        .or_else(|| models.get("currentModelId"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("catalog current_model_id missing from payload: {models:#?}"));
    let entries = models
        .get("models")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("catalog models array missing from payload: {models:#?}"));
    let ids = entries
        .iter()
        .filter_map(|entry| {
            entry
                .get("id")
                .or_else(|| entry.get("modelId"))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .collect::<Vec<_>>();
    (current_model_id, ids)
}

async fn seed_provider_status(daemon: &TestDaemon, status: ProviderStatus) {
    let provider_id = status.provider_id.clone();
    daemon.upsert_provider_status(provider_id, status).await;
}

#[tokio::test]
async fn live_gemini_model_catalog_matches_pinned_snapshot() {
    let Some(node_path) =
        required_absolute_existing_path("CTX_LIVE_GEMINI_NODE_PATH", "live Gemini node path")
    else {
        eprintln!("skipping: missing CTX_LIVE_GEMINI_NODE_PATH");
        return;
    };
    let Some(cli_entry_path) = required_absolute_existing_path(
        "CTX_LIVE_GEMINI_CLI_ENTRY_PATH",
        "live Gemini CLI entrypoint",
    ) else {
        eprintln!("skipping: missing CTX_LIVE_GEMINI_CLI_ENTRY_PATH");
        return;
    };
    let Some(bridge_path) = resolve_bridge_path() else {
        eprintln!("skipping: missing CTX_LIVE_ACP_CRP_BRIDGE_PATH and no acp-crp-bridge on PATH");
        return;
    };
    let Some(oauth_creds_json) = trimmed_env("CTX_LIVE_GEMINI_OAUTH_CREDS_JSON") else {
        eprintln!("skipping: missing CTX_LIVE_GEMINI_OAUTH_CREDS_JSON");
        return;
    };

    let expected_version = managed_gemini_version();
    let actual_version = live_gemini_cli_version(&node_path, &cli_entry_path);
    assert_eq!(
        actual_version, expected_version,
        "live Gemini CLI version drifted from the managed pinned version; update the pinned Gemini catalog intentionally"
    );

    let data_dir = tempfile::tempdir().expect("tempdir");
    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture_for_data_root_with_providers(
        data_dir.path(),
        HashMap::new(),
        "http://127.0.0.1:0",
    )
    .await;
    let app = fixture.router();

    add_gemini_account(
        data_dir.path(),
        Some("Live Gemini Catalog".to_string()),
        oauth_creds_json,
        trimmed_env("CTX_LIVE_GEMINI_GOOGLE_ACCOUNTS_JSON"),
        trimmed_env("CTX_LIVE_GEMINI_EMAIL"),
    )
    .await
    .expect("add live gemini account");

    let mut cfg = AgentServerConfigFile::default();
    cfg.providers.insert(
        "gemini".to_string(),
        AgentServerCommand {
            command: node_path,
            args: vec![cli_entry_path, "--experimental-acp".to_string()],
            dependencies: Vec::new(),
            managed: None,
        },
    );
    cfg.providers.insert(
        "acp-crp-bridge".to_string(),
        AgentServerCommand {
            command: bridge_path,
            args: Vec::new(),
            dependencies: Vec::new(),
            managed: None,
        },
    );
    save_agent_server_config(data_dir.path(), &cfg)
        .await
        .expect("save live gemini runtime config");

    seed_provider_status(
        &fixture.daemon,
        ProviderStatus {
            provider_id: "gemini".to_string(),
            installed: true,
            detected_path: None,
            version: Some(expected_version.clone()),
            capabilities: None,
            health: ProviderHealth::Ok,
            usability: ctx_providers::adapters::ProviderUsability::default(),
            diagnostics: Vec::new(),
            details: HashMap::new(),
        },
    )
    .await;

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (bootstrap_status, bootstrap): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        format!("/api/workspaces/{}/providers/bootstrap", ws.id.0),
        None,
    )
    .await;
    assert_eq!(
        bootstrap_status,
        StatusCode::OK,
        "bootstrap request failed: {bootstrap:#?}"
    );

    let pinned_models = bootstrap
        .pointer("/provider_options/gemini/models")
        .expect("pinned Gemini bootstrap models");
    let (expected_current_model_id, expected_ids) = catalog_snapshot(pinned_models);

    let (status, body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        format!("/api/workspaces/{}/providers/gemini/options", ws.id.0),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "options request failed: {body:#?}");
    assert_eq!(
        body.get("probe_ok").and_then(serde_json::Value::as_bool),
        Some(true),
        "expected live Gemini probe to succeed: {body:#?}"
    );
    assert_eq!(
        body.pointer("/models/meta/catalog_source")
            .and_then(serde_json::Value::as_str),
        Some("runtime_probe_live"),
        "expected live Gemini provider options to expose runtime_probe_live: {body:#?}"
    );

    let live_models = body.get("models").expect("live models payload");
    let (actual_current_model_id, actual_ids) = catalog_snapshot(live_models);

    assert_eq!(
        actual_current_model_id, expected_current_model_id,
        "live Gemini current_model_id drifted from the pinned snapshot; expected {expected_current_model_id:?}, got {actual_current_model_id:?}"
    );
    assert_eq!(
        actual_ids, expected_ids,
        "live Gemini model catalog drifted from the pinned snapshot; expected {expected_ids:#?}, got {actual_ids:#?}"
    );
}
