mod common;

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use axum::http::StatusCode;
use ctx_daemon::test_support::TestDaemon;
use ctx_harness_sources::{
    set_provider_source_selection, upsert_provider_endpoint, HarnessApiShape,
    HarnessEndpointUpsert, HarnessSourceKind,
};
use ctx_managed_installs::{
    load_agent_server_config, save_agent_server_config, AgentServerCommand, ManagedInstallMetadata,
};
use ctx_provider_accounts::{
    add_copilot_account, add_gemini_account, add_kimi_account, ensure_codex_account_dir,
    save_codex_registry, upsert_amp_account, CodexAccountEntry, CodexAccountRegistry,
    CodexEndpointProfile, CODEX_CREDENTIAL_KIND_API_KEY,
};
use ctx_provider_install::install_state::InstallTarget;
use ctx_providers::adapters::{ProviderAdapter, ProviderHealth, ProviderStatus};

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn lock_env() -> tokio::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().await
}

struct EnvVarGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prev }
    }

    fn without(key: &'static str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::remove_var(key);
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(prev) = self.prev.take() {
            std::env::set_var(self.key, prev);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

#[cfg(unix)]
fn write_executable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(path, contents).expect("write executable");
    let mut perms = std::fs::metadata(path).expect("metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("set permissions");
}

#[cfg(unix)]
fn write_fake_sandbox_cli(path: &Path) {
    write_executable(
        path,
        r#"#!/bin/sh
echo "fake sandbox CLI unavailable" >&2
exit 1
"#,
    );
}

#[cfg(unix)]
fn write_invalid_kimi_account_registry(data_root: &Path) {
    let path = ctx_provider_accounts::kimi_registry_path(data_root);
    std::fs::create_dir_all(path.parent().expect("kimi registry parent"))
        .expect("create kimi registry parent");
    std::fs::write(path, "{ not valid json").expect("write invalid kimi registry");
}

#[cfg(unix)]
async fn write_stale_codex_endpoint_selection(data_root: &Path) {
    let endpoint = upsert_provider_endpoint(
        data_root,
        "codex",
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "Codex endpoint".to_string(),
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
    .expect("upsert codex endpoint");
    set_provider_source_selection(
        data_root,
        "codex",
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select codex endpoint");

    let registry_path = data_root
        .join("providers")
        .join("harness_sources")
        .join("registry.json");
    let mut registry: serde_json::Value = serde_json::from_slice(
        &tokio::fs::read(&registry_path)
            .await
            .expect("read harness registry"),
    )
    .expect("parse harness registry");
    registry["providers"]["codex"]["endpoints"] = serde_json::json!([]);
    tokio::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&registry).expect("serialize harness registry"),
    )
    .await
    .expect("write stale harness registry");
}

#[cfg(unix)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn write_avf_probe_helper(path: &Path) {
    write_executable(
        path,
        r#"#!/bin/sh
set -eu
case "${1:-}" in
  probe)
    cat <<'JSON'
{"protocol_version":1,"protocol_schema":"ctx.avf_linux_helper.v1","helper_version":"test-helper","host_os":"macos","host_arch":"aarch64","supported":true,"save_restore_supported":true,"rosetta_supported":true,"notes":["ready"]}
JSON
    ;;
  *)
    echo "unsupported" >&2
    exit 1
    ;;
esac
"#,
    );
}

#[cfg(unix)]
fn setup_runtime_command_with_managed_interpreter_response(
    data_root: &Path,
    provider_id: &str,
    models_list_response: &str,
) -> (String, String) {
    let probe_response: serde_json::Value =
        serde_json::from_str(models_list_response).expect("parse probe response");
    setup_runtime_command_with_managed_interpreter_and_probe_response(
        data_root,
        provider_id,
        &probe_response,
    )
}

#[cfg(unix)]
fn setup_runtime_command_with_managed_interpreter_and_probe_response(
    data_root: &Path,
    provider_id: &str,
    probe_response: &serde_json::Value,
) -> (String, String) {
    let dep_bin_rel = format!("managed/runtime-node-{provider_id}/bin");
    let dep_bin_dir = data_root.join(&dep_bin_rel);
    std::fs::create_dir_all(&dep_bin_dir).expect("create dep bin dir");

    let probe_response_path = data_root.join(format!("{provider_id}-probe-models-response.json"));
    std::fs::write(
        &probe_response_path,
        serde_json::to_vec(probe_response).expect("serialize probe response"),
    )
    .expect("write probe response");

    let interpreter_name = format!("ctx-managed-probe-node-{provider_id}");
    let interpreter = dep_bin_dir.join(&interpreter_name);
    write_executable(
        &interpreter,
        &format!(
            r#"#!/bin/sh
if [ -n "${{CTX_AUTH_TOKEN:-}}" ]; then
  echo "unexpected CTX_AUTH_TOKEN in probe env" >&2
  exit 91
fi
while IFS= read -r line; do
  case "$line" in
    *models.list*)
      cat '{}'
      exit 0
      ;;
  esac
done
exit 1
        "#,
            probe_response_path.to_string_lossy()
        ),
    );

    let runtime_dir = data_root
        .join("providers")
        .join("agent-servers")
        .join(provider_id)
        .join("fixture")
        .join("dist")
        .join("bin");
    std::fs::create_dir_all(&runtime_dir).expect("create runtime dir");
    let runtime_cmd = runtime_dir.join(format!("{provider_id}-acp.js"));
    write_executable(
        &runtime_cmd,
        &format!("#!/usr/bin/env {interpreter_name}\n// fixture runtime\n"),
    );

    (runtime_cmd.to_string_lossy().to_string(), dep_bin_rel)
}

#[cfg(unix)]
fn setup_explicit_gemini_runtime_command_with_probe_response(
    data_root: &Path,
    probe_response: &serde_json::Value,
) -> AgentServerCommand {
    let probe_response_path = data_root.join("gemini-probe-models-response.json");
    std::fs::write(
        &probe_response_path,
        serde_json::to_vec(probe_response).expect("serialize gemini probe response"),
    )
    .expect("write gemini probe response");

    let node_bin = data_root
        .join("bundle")
        .join("runtimes")
        .join("node")
        .join("bin")
        .join("node");
    let cli_entry = data_root
        .join("bundle")
        .join("providers")
        .join("gemini")
        .join("node_modules")
        .join("@google")
        .join("gemini-cli")
        .join("bundle")
        .join("gemini.js");
    let core_entry = cli_entry
        .parent()
        .expect("bundle dir")
        .join("core-ctx-test.js");
    let cli_pkg = cli_entry
        .parent()
        .expect("bundle dir")
        .parent()
        .expect("cli root")
        .join("package.json");

    std::fs::create_dir_all(node_bin.parent().expect("node parent")).expect("mkdir node");
    std::fs::create_dir_all(cli_entry.parent().expect("cli parent")).expect("mkdir cli");
    write_executable(
        &node_bin,
        &format!(
            r#"#!/bin/sh
if [ -n "${{CTX_AUTH_TOKEN:-}}" ]; then
  echo "unexpected CTX_AUTH_TOKEN in probe env" >&2
  exit 91
fi
while IFS= read -r line; do
  case "$line" in
    *models.list*)
      cat '{}'
      exit 0
      ;;
  esac
done
exit 1
"#,
            probe_response_path.to_string_lossy()
        ),
    );
    std::fs::write(&cli_entry, b"cli").expect("write cli entry");
    std::fs::write(
        &core_entry,
        "export const coreEvents = {}; export const CoreEvent = {}; export const writeToStdout = () => {}; export const writeToStderr = () => {};",
    )
    .expect("write core entry");
    std::fs::write(
        &cli_pkg,
        r#"{"name":"@google/gemini-cli","version":"0.39.0"}"#,
    )
    .expect("write cli package");

    AgentServerCommand {
        command: node_bin.to_string_lossy().to_string(),
        args: vec![
            cli_entry.to_string_lossy().to_string(),
            "--experimental-acp".to_string(),
        ],
        dependencies: Vec::new(),
        managed: None,
    }
}

#[cfg(unix)]
fn setup_runtime_command_with_managed_interpreter(
    data_root: &Path,
    provider_id: &str,
) -> (String, String) {
    setup_runtime_command_with_managed_interpreter_response(
        data_root,
        provider_id,
        r#"{"seq":1,"channel":"control","type":"models.list","models":[{"id":"fixture-model"}],"current_model_id":"fixture-model","catalog_source":"live_remote"}"#,
    )
}

#[cfg(unix)]
fn setup_runtime_command_requiring_crp_handshake(
    data_root: &Path,
    provider_id: &str,
) -> (String, String, PathBuf) {
    let dep_bin_rel = format!("managed/runtime-node-{provider_id}/bin");
    std::fs::create_dir_all(data_root.join(&dep_bin_rel)).expect("create dep bin dir");

    let probe_response_path = data_root.join(format!("{provider_id}-handshake-response.json"));
    let handshake_marker_path = data_root.join(format!("{provider_id}-handshake-observed"));
    std::fs::write(
        &probe_response_path,
        r#"{"seq":1,"channel":"control","type":"models.list","models":[{"id":"fixture-model"}],"current_model_id":"fixture-model","catalog_source":"live_remote"}"#,
    )
    .expect("write handshake response");

    let runtime_dir = data_root
        .join("providers")
        .join("agent-servers")
        .join(provider_id)
        .join("handshake-required")
        .join("bin");
    std::fs::create_dir_all(&runtime_dir).expect("create runtime dir");
    let runtime_cmd = runtime_dir.join(format!("{provider_id}-crp"));
    write_executable(
        &runtime_cmd,
        &format!(
            r#"#!/bin/sh
if [ -n "${{CTX_AUTH_TOKEN:-}}" ]; then
  echo "unexpected CTX_AUTH_TOKEN in probe env" >&2
  exit 91
fi
if IFS= read -r line; then
  case "$line" in
    *models.list*)
      echo observed > '{}'
      cat '{}'
      exit 0
      ;;
  esac
fi
exit 1
"#,
            handshake_marker_path.to_string_lossy(),
            probe_response_path.to_string_lossy()
        ),
    );

    (
        runtime_cmd.to_string_lossy().to_string(),
        dep_bin_rel,
        handshake_marker_path,
    )
}

async fn provider_probe_fixture(data_dir: tempfile::TempDir) -> common::FakeDaemonFixture {
    let providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    common::fake_daemon_fixture_in_data_dir_with_providers(
        data_dir,
        providers,
        "http://127.0.0.1:0",
    )
    .await
}

async fn seed_provider_status(state: &TestDaemon, status: ProviderStatus) {
    let provider_id = status.provider_id.clone();
    state.upsert_provider_status(provider_id, status).await;
}

async fn seed_runtime_and_status(
    state: &TestDaemon,
    provider_id: &str,
    runtime_cmd: String,
    dep_bin_rel: String,
) {
    let dep_id = format!("runtime-node-host-{provider_id}");
    let mut cfg = load_agent_server_config(state.data_root())
        .await
        .unwrap_or_default();
    cfg.providers.insert(
        provider_id.to_string(),
        AgentServerCommand {
            command: runtime_cmd,
            args: Vec::new(),
            dependencies: vec![dep_id.clone()],
            managed: None,
        },
    );
    cfg.managed_installs.insert(
        dep_id,
        ManagedInstallMetadata {
            package: Some("node-runtime".to_string()),
            version: Some("fixture".to_string()),
            archive_sha256: None,
            artifact_fingerprint: None,
            target: None,
            install_dir_rel: None,
            bin_dir_rel: Some(dep_bin_rel),
            last_success_at: None,
            last_error: None,
        },
    );
    save_agent_server_config(state.data_root(), &cfg)
        .await
        .expect("save runtime config");

    seed_provider_status(
        state,
        ProviderStatus {
            provider_id: provider_id.to_string(),
            installed: true,
            detected_path: None,
            version: None,
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ctx_providers::adapters::ProviderUsability::default(),
        },
    )
    .await;
}

#[cfg(unix)]
async fn seed_managed_codex_cli_dependency(state: &TestDaemon, dep_bin_rel: &str) {
    let dep_bin_dir = state.data_root().join(dep_bin_rel);
    std::fs::create_dir_all(&dep_bin_dir).expect("create codex-cli dep bin dir");
    let codex_cmd = dep_bin_dir.join("codex");
    write_executable(
        &codex_cmd,
        r#"#!/bin/sh
exit 0
"#,
    );

    let mut cfg = load_agent_server_config(state.data_root())
        .await
        .unwrap_or_default();
    cfg.managed_provider_targets.insert(
        "codex-cli".to_string(),
        HashMap::from([(
            "host".to_string(),
            AgentServerCommand {
                command: codex_cmd.to_string_lossy().to_string(),
                args: Vec::new(),
                dependencies: Vec::new(),
                managed: None,
            },
        )]),
    );
    cfg.managed_installs.insert(
        "codex-cli".to_string(),
        ManagedInstallMetadata {
            package: Some("codex-cli".to_string()),
            version: Some("fixture".to_string()),
            archive_sha256: None,
            artifact_fingerprint: None,
            target: Some(InstallTarget::Host),
            install_dir_rel: None,
            bin_dir_rel: Some(dep_bin_rel.to_string()),
            last_success_at: None,
            last_error: None,
        },
    );
    save_agent_server_config(state.data_root(), &cfg)
        .await
        .expect("save codex-cli managed dependency");
}

#[cfg(unix)]
async fn seed_acp_bridge_runtime(data_root: &Path) {
    let bridge_dir = data_root
        .join("providers")
        .join("agent-servers")
        .join("acp-crp-bridge")
        .join("fixture")
        .join("bin");
    std::fs::create_dir_all(&bridge_dir).expect("create bridge runtime dir");

    let bridge_cmd = bridge_dir.join("acp-crp-bridge");
    write_executable(
        &bridge_cmd,
        r#"#!/bin/sh
while [ "$#" -gt 0 ]; do
  case "$1" in
    --acp-command)
      shift
      exec /bin/sh -lc "$1"
      ;;
    *)
      shift
      ;;
  esac
done
echo "missing --acp-command" >&2
exit 1
"#,
    );

    let mut cfg = ctx_managed_installs::load_agent_server_config(data_root)
        .await
        .unwrap_or_default();
    cfg.providers.insert(
        "acp-crp-bridge".to_string(),
        AgentServerCommand {
            command: bridge_cmd.to_string_lossy().to_string(),
            args: Vec::new(),
            dependencies: Vec::new(),
            managed: None,
        },
    );
    save_agent_server_config(data_root, &cfg)
        .await
        .expect("save bridge runtime config");
}

#[cfg(unix)]
async fn configure_hermetic_codex_host_auth(root: &Path) -> Vec<EnvVarGuard> {
    let host_auth = root.join("fixture-codex-auth.json");
    tokio::fs::write(&host_auth, br#"{"OPENAI_API_KEY":"fixture-codex-key"}"#)
        .await
        .expect("write fixture codex auth");
    vec![
        EnvVarGuard::without("CTX_CODEX_HOME"),
        EnvVarGuard::without("CTX_SEED_CODEX_AUTH_FROM_HOST"),
        EnvVarGuard::set(
            "CTX_CODEX_HOST_AUTH_PATH",
            host_auth.to_string_lossy().as_ref(),
        ),
    ]
}

#[cfg(unix)]
async fn seed_active_codex_subscription_account(data_root: &Path) {
    let account_id = "fixture-codex-account";
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "Fixture Codex".to_string(),
            kind: CODEX_CREDENTIAL_KIND_API_KEY.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: chrono::Utc::now(),
            last_used_at: None,
            secret_ref: None,
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(data_root, &registry)
        .await
        .expect("save fixture codex account registry");
    let account_dir = ensure_codex_account_dir(data_root, account_id)
        .await
        .expect("create fixture codex account dir");
    tokio::fs::write(
        account_dir.join("auth.json"),
        br#"{"OPENAI_API_KEY":"fixture-codex-key"}"#,
    )
    .await
    .expect("write fixture codex account auth");
}

#[cfg(unix)]
#[tokio::test]
async fn provider_options_probe_uses_managed_dependency_path() {
    let _env_lock = lock_env().await;
    let data_dir = tempfile::tempdir().expect("tempdir");
    let _env_guards = configure_hermetic_codex_host_auth(data_dir.path()).await;
    seed_active_codex_subscription_account(data_dir.path()).await;
    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    let fixture = provider_probe_fixture(data_dir).await;
    let state = &fixture.daemon;
    let app = fixture.router();

    let (runtime_cmd, dep_bin_rel) =
        setup_runtime_command_with_managed_interpreter(fixture.data_dir.path(), "codex");
    seed_runtime_and_status(&state, "codex", runtime_cmd, dep_bin_rel).await;
    seed_managed_codex_cli_dependency(&state, "managed/runtime-node-codex/bin").await;

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (status, body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        format!("/api/workspaces/{}/providers/codex/options", ws.id.0),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "options request failed: {body:#?}");
    assert_eq!(
        body.get("probe_ok").and_then(serde_json::Value::as_bool),
        Some(true),
        "expected probe_ok=true with managed runtime path injection: {body:#?}"
    );
    assert_eq!(
        body.pointer("/models/current_model_id")
            .and_then(serde_json::Value::as_str),
        Some("fixture-model"),
        "expected fixture model probe result: {body:#?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn provider_options_preserve_live_runtime_catalog_for_preferred_models() {
    let _env_lock = lock_env().await;
    let data_dir = tempfile::tempdir().expect("tempdir");
    let _env_guards = configure_hermetic_codex_host_auth(data_dir.path()).await;
    seed_active_codex_subscription_account(data_dir.path()).await;
    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    let fixture = provider_probe_fixture(data_dir).await;
    let state = &fixture.daemon;
    let app = fixture.router();

    let (runtime_cmd, dep_bin_rel) = setup_runtime_command_with_managed_interpreter_response(
        fixture.data_dir.path(),
        "codex",
        r#"{"seq":1,"channel":"control","type":"models.list","models":[{"id":"runtime-live"},{"id":"runtime-only"}],"current_model_id":"runtime-live","catalog_source":"live_remote"}"#,
    );
    seed_runtime_and_status(&state, "codex", runtime_cmd, dep_bin_rel).await;
    seed_managed_codex_cli_dependency(&state, "managed/runtime-node-codex/bin").await;

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (pref_status, pref_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        format!(
            "/api/workspaces/{}/provider_model_preferences/codex",
            ws.id.0
        ),
        Some(serde_json::json!({
            "preferred_model_id": "runtime-only"
        })),
    )
    .await;
    assert_eq!(
        pref_status,
        StatusCode::OK,
        "preference request failed: {pref_body:#?}"
    );

    let (status, body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        format!("/api/workspaces/{}/providers/codex/options", ws.id.0),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "options request failed: {body:#?}");
    assert_eq!(
        body.pointer("/models/current_model_id")
            .and_then(serde_json::Value::as_str),
        Some("runtime-live"),
        "expected runtime-probed current model to win: {body:#?}"
    );
    assert_eq!(
        body.get("preferred_model_id")
            .and_then(serde_json::Value::as_str),
        Some("runtime-only"),
        "expected runtime-only preferred model to remain visible: {body:#?}"
    );
    let available_models = body
        .pointer("/models/models")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        available_models.iter().any(|model| {
            model.get("id").and_then(serde_json::Value::as_str) == Some("runtime-only")
        }),
        "expected live runtime catalog to include runtime-only model: {body:#?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn kimi_provider_options_expose_live_runtime_catalog_and_bootstrap_stays_hydratable() {
    let _env_lock = lock_env().await;
    let data_dir = tempfile::tempdir().expect("tempdir");
    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    let fixture = provider_probe_fixture(data_dir).await;
    let state = &fixture.daemon;
    let app = fixture.router();

    add_kimi_account(
        fixture.data_dir.path(),
        Some("Kimi Test".to_string()),
        None,
        r#"{"api_key":"kimi-key"}"#.to_string(),
        None,
        Some("kimi@example.com".to_string()),
    )
    .await
    .expect("add kimi account");

    let (bridge_cmd, bridge_dep_bin_rel) =
        setup_runtime_command_with_managed_interpreter(fixture.data_dir.path(), "acp-crp-bridge");
    seed_runtime_and_status(&state, "acp-crp-bridge", bridge_cmd, bridge_dep_bin_rel).await;
    let (runtime_cmd, dep_bin_rel) =
        setup_runtime_command_with_managed_interpreter(fixture.data_dir.path(), "kimi");
    seed_runtime_and_status(&state, "kimi", runtime_cmd, dep_bin_rel).await;

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (status, options): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        format!("/api/workspaces/{}/providers/kimi/options", ws.id.0),
        None,
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "options request failed: {options:#?}"
    );
    assert_eq!(
        options.get("probe_ok").and_then(serde_json::Value::as_bool),
        Some(true),
        "expected probe_ok=true for kimi runtime discovery: {options:#?}"
    );
    assert_eq!(
        options
            .get("has_active_auth")
            .and_then(serde_json::Value::as_bool),
        Some(true),
        "expected active kimi auth to remain visible in provider options: {options:#?}"
    );
    assert_eq!(
        options
            .pointer("/models/current_model_id")
            .and_then(serde_json::Value::as_str),
        Some("fixture-model"),
        "expected kimi provider options to expose the live runtime model: {options:#?}"
    );
    assert_eq!(
        options
            .pointer("/models/meta/catalog_source")
            .and_then(serde_json::Value::as_str),
        Some("runtime_probe_live"),
        "expected kimi provider options to advertise a live runtime catalog: {options:#?}"
    );

    let (status, bootstrap): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        format!("/api/workspaces/{}/providers/bootstrap", ws.id.0),
        None,
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "bootstrap request failed: {bootstrap:#?}"
    );
    assert_eq!(
        bootstrap
            .pointer("/provider_options/kimi/has_active_auth")
            .and_then(serde_json::Value::as_bool),
        Some(true),
        "expected bootstrap to keep kimi marked as authenticated for web hydration: {bootstrap:#?}"
    );
    assert_eq!(
        bootstrap
            .pointer("/provider_options/kimi/source/selected_source_kind")
            .and_then(serde_json::Value::as_str),
        Some("subscription"),
        "expected bootstrap to keep kimi on the subscription discovery path: {bootstrap:#?}"
    );
    assert_ne!(
        bootstrap
            .pointer("/provider_options/kimi/models/meta/catalog_source")
            .and_then(serde_json::Value::as_str),
        Some("runtime_probe_live"),
        "bootstrap must not expose kimi's live runtime model catalog: {bootstrap:#?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn kimi_provider_options_fail_closed_on_account_registry_errors() {
    let _env_lock = lock_env().await;
    let data_dir = tempfile::tempdir().expect("tempdir");
    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    write_invalid_kimi_account_registry(data_dir.path());

    let fixture = provider_probe_fixture(data_dir).await;
    let app = fixture.router();
    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (status, body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        format!("/api/workspaces/{}/providers/kimi/options", ws.id.0),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "options request failed: {body:#?}");
    assert_eq!(
        body.get("probe_ok").and_then(serde_json::Value::as_bool),
        Some(false),
        "expected probe_ok=false for invalid kimi account registry: {body:#?}"
    );
    assert_eq!(
        body.get("has_active_auth")
            .and_then(serde_json::Value::as_bool),
        Some(false),
        "expected has_active_auth=false for invalid kimi account registry: {body:#?}"
    );
    assert_eq!(
        body.get("auth_mode").and_then(serde_json::Value::as_str),
        Some("none"),
        "expected auth_mode=none for invalid kimi account registry: {body:#?}"
    );
    assert!(
        body.get("config_error")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| value.contains("Kimi account registry")),
        "expected kimi registry parse context in options response: {body:#?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn provider_bootstrap_fails_closed_on_kimi_account_registry_errors() {
    let _env_lock = lock_env().await;
    let data_dir = tempfile::tempdir().expect("tempdir");
    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    write_invalid_kimi_account_registry(data_dir.path());

    let fixture = provider_probe_fixture(data_dir).await;
    let app = fixture.router();
    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (status, body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        format!("/api/workspaces/{}/providers/bootstrap", ws.id.0),
        None,
    )
    .await;

    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "bootstrap request unexpectedly succeeded: {body:#?}"
    );
    assert!(
        body.get("error")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| value.contains("failed to load kimi accounts")),
        "expected kimi registry load failure in bootstrap response: {body:#?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn codex_provider_options_surface_stale_selected_endpoint_errors() {
    let _env_lock = lock_env().await;
    let data_dir = tempfile::tempdir().expect("tempdir");
    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    write_stale_codex_endpoint_selection(data_dir.path()).await;

    let fixture = provider_probe_fixture(data_dir).await;
    let state = &fixture.daemon;
    seed_provider_status(
        &state,
        ProviderStatus {
            provider_id: "codex".to_string(),
            installed: true,
            detected_path: None,
            version: None,
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ctx_providers::adapters::ProviderUsability::default(),
        },
    )
    .await;
    let app = fixture.router();
    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (status, body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        format!("/api/workspaces/{}/providers/codex/options", ws.id.0),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "options request failed: {body:#?}");
    assert_eq!(
        body.get("probe_ok").and_then(serde_json::Value::as_bool),
        Some(false),
        "expected probe_ok=false for stale selected endpoint: {body:#?}"
    );
    assert!(
        body.get("config_error")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| value.contains("selected endpoint")),
        "expected stale selected endpoint error in options response: {body:#?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn provider_bootstrap_surfaces_stale_selected_endpoint_errors() {
    let _env_lock = lock_env().await;
    let data_dir = tempfile::tempdir().expect("tempdir");
    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    write_stale_codex_endpoint_selection(data_dir.path()).await;

    let fixture = provider_probe_fixture(data_dir).await;
    let state = &fixture.daemon;
    seed_provider_status(
        &state,
        ProviderStatus {
            provider_id: "codex".to_string(),
            installed: true,
            detected_path: None,
            version: None,
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ctx_providers::adapters::ProviderUsability::default(),
        },
    )
    .await;
    let app = fixture.router();
    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (status, body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        format!("/api/workspaces/{}/providers/bootstrap", ws.id.0),
        None,
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "bootstrap request failed unexpectedly: {body:#?}"
    );
    assert!(
        body.pointer("/provider_options/codex/config_error")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| value.contains("selected endpoint")),
        "expected stale selected endpoint error in bootstrap response: {body:#?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn amp_provider_options_include_live_runtime_model_catalog() {
    let _env_lock = lock_env().await;
    let data_dir = tempfile::tempdir().expect("tempdir");
    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    let fixture = provider_probe_fixture(data_dir).await;
    let state = &fixture.daemon;
    let app = fixture.router();

    upsert_amp_account(
        fixture.data_dir.path(),
        Some("Amp Test".to_string()),
        Some("amp@example.com".to_string()),
    )
    .await
    .expect("upsert amp account");

    let (bridge_cmd, bridge_dep_bin_rel) =
        setup_runtime_command_with_managed_interpreter(fixture.data_dir.path(), "acp-crp-bridge");
    seed_runtime_and_status(&state, "acp-crp-bridge", bridge_cmd, bridge_dep_bin_rel).await;
    let (runtime_cmd, dep_bin_rel) =
        setup_runtime_command_with_managed_interpreter(fixture.data_dir.path(), "amp");
    seed_runtime_and_status(&state, "amp", runtime_cmd, dep_bin_rel).await;

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (status, body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        format!("/api/workspaces/{}/providers/amp/options", ws.id.0),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "options request failed: {body:#?}");
    assert_eq!(
        body.get("probe_ok").and_then(serde_json::Value::as_bool),
        Some(true),
        "expected probe_ok=true for amp live discovery fixture: {body:#?}"
    );
    assert_eq!(
        body.get("has_active_auth")
            .and_then(serde_json::Value::as_bool),
        Some(true),
        "expected has_active_auth=true for active amp account: {body:#?}"
    );
    assert_eq!(
        body.pointer("/models/current_model_id")
            .and_then(serde_json::Value::as_str),
        Some("fixture-model"),
        "expected fixture model probe result for amp: {body:#?}"
    );
    assert_eq!(
        body.pointer("/models/meta/catalog_source")
            .and_then(serde_json::Value::as_str),
        Some("runtime_probe_live"),
        "expected live runtime model catalog for amp: {body:#?}"
    );

    let (status, bootstrap): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        format!("/api/workspaces/{}/providers/bootstrap", ws.id.0),
        None,
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "bootstrap request failed: {bootstrap:#?}"
    );
    assert_eq!(
        bootstrap
            .pointer("/provider_options/amp/has_active_auth")
            .and_then(serde_json::Value::as_bool),
        Some(true),
        "expected bootstrap to keep amp marked as authenticated for web hydration: {bootstrap:#?}"
    );
    assert_eq!(
        bootstrap
            .pointer("/provider_options/amp/auth_mode")
            .and_then(serde_json::Value::as_str),
        Some("subscription"),
        "expected bootstrap to keep amp on subscription auth: {bootstrap:#?}"
    );
    assert_ne!(
        bootstrap
            .pointer("/provider_options/amp/models/meta/catalog_source")
            .and_then(serde_json::Value::as_str),
        Some("runtime_probe_live"),
        "bootstrap must not expose amp's live runtime model catalog: {bootstrap:#?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn amp_provider_options_fail_when_runtime_probe_returns_no_live_model_catalog() {
    let _env_lock = lock_env().await;
    let data_dir = tempfile::tempdir().expect("tempdir");
    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    let fixture = provider_probe_fixture(data_dir).await;
    let state = &fixture.daemon;
    let app = fixture.router();

    upsert_amp_account(
        fixture.data_dir.path(),
        Some("Amp Test".to_string()),
        Some("amp@example.com".to_string()),
    )
    .await
    .expect("upsert amp account");

    let (bridge_cmd, bridge_dep_bin_rel) = setup_runtime_command_with_managed_interpreter_response(
        fixture.data_dir.path(),
        "acp-crp-bridge",
        r#"{"seq":1,"channel":"control","type":"models.list","models":[],"current_model_id":null}"#,
    );
    seed_runtime_and_status(&state, "acp-crp-bridge", bridge_cmd, bridge_dep_bin_rel).await;
    let (runtime_cmd, dep_bin_rel) =
        setup_runtime_command_with_managed_interpreter(fixture.data_dir.path(), "amp");
    seed_runtime_and_status(&state, "amp", runtime_cmd, dep_bin_rel).await;

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (status, body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        format!("/api/workspaces/{}/providers/amp/options", ws.id.0),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "options request failed: {body:#?}");
    assert_eq!(
        body.get("probe_ok").and_then(serde_json::Value::as_bool),
        Some(false),
        "expected probe_ok=false when amp returns no live model catalog: {body:#?}"
    );
    assert!(
        body.get("probe_error")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|message| {
                message.contains("runtime_model_catalog_missing: provider=amp")
            }),
        "expected explicit missing model catalog probe error for amp: {body:#?}"
    );
    assert!(
        body.get("models").is_none() || body.get("models").is_some_and(serde_json::Value::is_null),
        "expected amp options without a live model catalog to omit models: {body:#?}"
    );
}

#[tokio::test]
async fn copilot_provider_options_include_pinned_model_catalog_when_live_probe_is_unavailable() {
    let _env_lock = lock_env().await;
    let data_dir = tempfile::tempdir().expect("tempdir");
    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    let fixture = provider_probe_fixture(data_dir).await;
    let state = &fixture.daemon;
    let app = fixture.router();

    add_copilot_account(
        fixture.data_dir.path(),
        Some("Copilot Test".to_string()),
        "gho_fixture_token".to_string(),
        Some("copilot@example.com".to_string()),
    )
    .await
    .expect("add copilot account");

    let (bridge_cmd, bridge_dep_bin_rel) =
        setup_runtime_command_with_managed_interpreter(fixture.data_dir.path(), "acp-crp-bridge");
    seed_runtime_and_status(&state, "acp-crp-bridge", bridge_cmd, bridge_dep_bin_rel).await;

    seed_provider_status(
        &state,
        ProviderStatus {
            provider_id: "copilot".to_string(),
            installed: true,
            detected_path: None,
            version: Some("1.0.3".to_string()),
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ctx_providers::adapters::ProviderUsability::default(),
        },
    )
    .await;

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (status, body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        format!("/api/workspaces/{}/providers/copilot/options", ws.id.0),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "options request failed: {body:#?}");
    assert_eq!(
        body.get("probe_ok").and_then(serde_json::Value::as_bool),
        Some(false),
        "expected probe_ok=false when the copilot runtime command is unavailable: {body:#?}"
    );
    assert!(
        body.get("probe_error")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|message| message.contains("runtime_command_missing: provider=copilot")),
        "expected runtime command probe error for copilot fixture: {body:#?}"
    );
    assert_eq!(
        body.get("has_active_auth")
            .and_then(serde_json::Value::as_bool),
        Some(true),
        "expected has_active_auth=true for active copilot account: {body:#?}"
    );
    assert_eq!(
        body.pointer("/models/catalog_source")
            .and_then(serde_json::Value::as_str),
        Some("copilot_version_pinned"),
        "expected pinned copilot model catalog in provider options: {body:#?}"
    );
    assert_eq!(
        body.pointer("/models/current_model_id")
            .and_then(serde_json::Value::as_str),
        Some("gpt-5-mini"),
        "expected bootstrap-safe copilot model in provider options: {body:#?}"
    );
    assert_eq!(
        body.pointer("/models/default_model_id")
            .and_then(serde_json::Value::as_str),
        Some("claude-sonnet-4.6"),
        "expected default copilot model in provider options: {body:#?}"
    );
}

#[tokio::test]
async fn providers_bootstrap_includes_pinned_codex_claude_and_gemini_catalogs() {
    let _env_lock = lock_env().await;
    let data_dir = tempfile::tempdir().expect("tempdir");
    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    let fixture = provider_probe_fixture(data_dir).await;
    let state = &fixture.daemon;
    let app = fixture.router();

    seed_provider_status(
        &state,
        ProviderStatus {
            provider_id: "codex".to_string(),
            installed: true,
            detected_path: None,
            version: Some("0.98.0".to_string()),
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ctx_providers::adapters::ProviderUsability::default(),
        },
    )
    .await;
    seed_provider_status(
        &state,
        ProviderStatus {
            provider_id: "claude-crp".to_string(),
            installed: true,
            detected_path: None,
            version: Some("2.1.47".to_string()),
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ctx_providers::adapters::ProviderUsability::default(),
        },
    )
    .await;
    seed_provider_status(
        &state,
        ProviderStatus {
            provider_id: "gemini".to_string(),
            installed: true,
            detected_path: None,
            version: Some("0.39.0".to_string()),
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ctx_providers::adapters::ProviderUsability::default(),
        },
    )
    .await;

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (status, body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        format!("/api/workspaces/{}/providers/bootstrap", ws.id.0),
        None,
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "bootstrap request failed: {body:#?}"
    );
    assert_eq!(
        body.pointer("/provider_options/codex/models/meta/catalog_source")
            .and_then(serde_json::Value::as_str),
        Some("codex_bundle_pinned"),
        "expected pinned codex bootstrap catalog: {body:#?}"
    );
    assert_eq!(
        body.pointer("/provider_options/codex/models/current_model_id")
            .and_then(serde_json::Value::as_str),
        Some("gpt-5.4/medium"),
        "expected pinned codex bootstrap current model: {body:#?}"
    );
    assert_eq!(
        body.pointer("/provider_options/claude-crp/models/meta/catalog_source")
            .and_then(serde_json::Value::as_str),
        Some("claude_subscription_pinned"),
        "expected pinned claude bootstrap catalog: {body:#?}"
    );
    assert_eq!(
        body.pointer("/provider_options/claude-crp/models/current_model_id")
            .and_then(serde_json::Value::as_str),
        Some("default/medium"),
        "expected pinned claude bootstrap current model: {body:#?}"
    );
    assert_eq!(
        body.pointer("/provider_options/gemini/models/meta/catalog_source")
            .and_then(serde_json::Value::as_str),
        Some("gemini_cli_version_pinned"),
        "expected pinned gemini bootstrap catalog: {body:#?}"
    );
    assert_eq!(
        body.pointer("/provider_options/gemini/models/catalog_version")
            .and_then(serde_json::Value::as_str),
        Some("0.39.0"),
        "expected pinned gemini bootstrap catalog version: {body:#?}"
    );
    assert_eq!(
        body.pointer("/provider_options/gemini/models/current_model_id")
            .and_then(serde_json::Value::as_str),
        Some("auto-gemini-3"),
        "expected pinned gemini bootstrap current model: {body:#?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn gemini_provider_options_use_live_acp_catalog_when_probe_succeeds() {
    let _env_lock = lock_env().await;
    let data_dir = tempfile::tempdir().expect("tempdir");
    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    let fixture = provider_probe_fixture(data_dir).await;
    let state = &fixture.daemon;
    let app = fixture.router();

    add_gemini_account(
        fixture.data_dir.path(),
        Some("Gemini Test".to_string()),
        r#"{"access_token":"fixture-gemini-token","refresh_token":"fixture-refresh-token"}"#
            .to_string(),
        None,
        Some("gemini@example.com".to_string()),
    )
    .await
    .expect("add gemini account");

    let gemini_cmd = setup_explicit_gemini_runtime_command_with_probe_response(
        fixture.data_dir.path(),
        &serde_json::json!({
            "seq": 1,
            "channel": "control",
            "type": "models.list",
            "models": [
                { "id": "auto-gemini-3", "name": "Auto (Gemini 3)" },
                { "id": "auto-gemini-2.5", "name": "Auto (Gemini 2.5)" },
                { "id": "gemini-3-pro-preview", "name": "Gemini 3 Pro Preview" },
                { "id": "gemini-3-flash-preview", "name": "Gemini 3 Flash Preview" },
                { "id": "gemini-2.5-pro", "name": "Gemini 2.5 Pro" },
                { "id": "gemini-2.5-flash", "name": "Gemini 2.5 Flash" },
                { "id": "gemini-2.5-flash-lite", "name": "Gemini 2.5 Flash Lite" }
            ]
        }),
    );
    let mut cfg = ctx_managed_installs::load_agent_server_config(fixture.data_dir.path())
        .await
        .unwrap_or_default();
    cfg.providers.insert("gemini".to_string(), gemini_cmd);
    save_agent_server_config(fixture.data_dir.path(), &cfg)
        .await
        .expect("save gemini runtime config");
    seed_acp_bridge_runtime(fixture.data_dir.path()).await;
    seed_provider_status(
        &state,
        ProviderStatus {
            provider_id: "gemini".to_string(),
            installed: true,
            detected_path: None,
            version: Some("0.33.1".to_string()),
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ctx_providers::adapters::ProviderUsability::default(),
        },
    )
    .await;

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
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
        "expected gemini probe_ok=true with fixture ACP runtime: {body:#?}"
    );
    assert_eq!(
        body.pointer("/models/meta/catalog_source")
            .and_then(serde_json::Value::as_str),
        Some("runtime_probe_live"),
        "expected live ACP Gemini catalog to replace the pinned bootstrap catalog: {body:#?}"
    );
    assert_eq!(
        body.pointer("/models/current_model_id")
            .and_then(serde_json::Value::as_str),
        Some("auto-gemini-3"),
        "expected live ACP current model id for Gemini: {body:#?}"
    );
    assert_eq!(
        body.pointer("/models/models/6/id")
            .and_then(serde_json::Value::as_str),
        Some("gemini-2.5-flash-lite"),
        "expected full Gemini ACP model list to be preserved: {body:#?}"
    );
}

#[tokio::test]
async fn fake_provider_bootstrap_and_options_are_ready_without_browser_rewrite() {
    let _env_lock = lock_env().await;
    let _show_fake = EnvVarGuard::set("CTX_SHOW_FAKE_PROVIDER", "1");
    let data_dir = tempfile::tempdir().expect("tempdir");
    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    let fixture = provider_probe_fixture(data_dir).await;
    let state = &fixture.daemon;
    let app = fixture.router();

    seed_provider_status(
        &state,
        ProviderStatus {
            provider_id: "fake".to_string(),
            installed: true,
            detected_path: None,
            version: None,
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ctx_providers::adapters::ProviderUsability::default(),
        },
    )
    .await;
    let mut hidden_details = HashMap::new();
    hidden_details.insert("ui_hidden".to_string(), "true".to_string());
    seed_provider_status(
        &state,
        ProviderStatus {
            provider_id: "codex".to_string(),
            installed: true,
            detected_path: None,
            version: None,
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: hidden_details,
            usability: ctx_providers::adapters::ProviderUsability::default(),
        },
    )
    .await;

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (status, options): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        format!("/api/workspaces/{}/providers/fake/options", ws.id.0),
        None,
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "options request failed: {options:#?}"
    );
    assert_eq!(
        options
            .get("has_active_auth")
            .and_then(serde_json::Value::as_bool),
        Some(true),
        "expected fake provider to advertise active auth readiness: {options:#?}"
    );
    assert_eq!(
        options.get("probe_ok").and_then(serde_json::Value::as_bool),
        Some(true),
        "expected fake provider probe to succeed: {options:#?}"
    );
    assert_eq!(
        options
            .pointer("/models/current_model_id")
            .and_then(serde_json::Value::as_str),
        Some("fake-model"),
        "expected fake provider to expose a concrete model id: {options:#?}"
    );
    assert_eq!(
        options
            .pointer("/models/meta/catalog_source")
            .and_then(serde_json::Value::as_str),
        Some("fake_provider"),
        "expected fake provider model catalog metadata: {options:#?}"
    );

    let (status, bootstrap): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        format!("/api/workspaces/{}/providers/bootstrap", ws.id.0),
        None,
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "bootstrap request failed: {bootstrap:#?}"
    );
    assert!(
        bootstrap
            .get("providers")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|providers| providers.iter().any(|provider| {
                provider
                    .get("provider_id")
                    .and_then(serde_json::Value::as_str)
                    == Some("fake")
            })),
        "expected fake provider to be present in bootstrap providers: {bootstrap:#?}"
    );
    let fake_provider = bootstrap
        .get("providers")
        .and_then(serde_json::Value::as_array)
        .and_then(|providers| {
            providers.iter().find(|provider| {
                provider
                    .get("provider_id")
                    .and_then(serde_json::Value::as_str)
                    == Some("fake")
            })
        })
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    assert_eq!(
        fake_provider
            .pointer("/details/ready_for_use")
            .and_then(serde_json::Value::as_str),
        Some("true"),
        "expected fake provider to be marked ready_for_use in bootstrap statuses: {bootstrap:#?}"
    );
    assert_eq!(
        bootstrap
            .pointer("/provider_options/fake/has_active_auth")
            .and_then(serde_json::Value::as_bool),
        Some(true),
        "expected fake bootstrap options to advertise active auth readiness: {bootstrap:#?}"
    );
    assert_eq!(
        bootstrap
            .pointer("/provider_options/fake/models/current_model_id")
            .and_then(serde_json::Value::as_str),
        Some("fake-model"),
        "expected fake bootstrap options to expose the concrete fake model: {bootstrap:#?}"
    );
    assert!(
        bootstrap
            .get("providers")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|providers| providers.iter().any(|provider| {
                provider
                    .get("provider_id")
                    .and_then(serde_json::Value::as_str)
                    == Some("codex")
                    && provider
                        .pointer("/details/ui_hidden")
                        .and_then(|v| v.as_str())
                        == Some("true")
            })),
        "hidden providers should remain in top-level bootstrap statuses: {bootstrap:#?}"
    );
    assert!(
        bootstrap.pointer("/provider_options/codex").is_none(),
        "hidden providers must not receive bootstrap options: {bootstrap:#?}"
    );
    assert!(
        bootstrap
            .pointer("/provider_harness_config/codex")
            .is_none(),
        "hidden providers must not receive bootstrap harness config: {bootstrap:#?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn provider_verify_probe_uses_managed_dependency_path() {
    let _env_lock = lock_env().await;
    let data_dir = tempfile::tempdir().expect("tempdir");
    let _env_guards = configure_hermetic_codex_host_auth(data_dir.path()).await;
    seed_active_codex_subscription_account(data_dir.path()).await;
    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    let fixture = provider_probe_fixture(data_dir).await;
    let state = &fixture.daemon;
    let app = fixture.router();

    let (runtime_cmd, dep_bin_rel) =
        setup_runtime_command_with_managed_interpreter(fixture.data_dir.path(), "codex");
    seed_runtime_and_status(&state, "codex", runtime_cmd, dep_bin_rel).await;
    seed_managed_codex_cli_dependency(&state, "managed/runtime-node-codex/bin").await;

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (status, body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        format!("/api/workspaces/{}/providers/codex/verify", ws.id.0),
        Some(serde_json::json!({})),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "verify request failed: {body:#?}");
    assert_eq!(
        body.get("status").and_then(serde_json::Value::as_str),
        Some("ok"),
        "expected verify status ok with managed runtime path injection: {body:#?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn provider_verify_selected_endpoint_uses_crp_handshake() {
    let _env_lock = lock_env().await;
    let data_dir = tempfile::tempdir().expect("tempdir");
    let _env_guards = configure_hermetic_codex_host_auth(data_dir.path()).await;
    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    let model_server =
        common::openai_responses_stub::spawn_openai_responses_sse_stub(Vec::new()).await;
    let endpoint = upsert_provider_endpoint(
        data_dir.path(),
        "codex",
        HarnessEndpointUpsert {
            endpoint_id: None,
            name: "Codex endpoint".to_string(),
            base_url: Some(format!("{}/v1", model_server.base_url)),
            api_shape: Some(HarnessApiShape::OpenaiResponses),
            auth_type: None,
            model_override: Some("mock-model".to_string()),
            api_key: Some("sk-test".to_string()),
            service_account_json: None,
            project_id: None,
            location: None,
        },
    )
    .await
    .expect("upsert codex endpoint");
    set_provider_source_selection(
        data_dir.path(),
        "codex",
        HarnessSourceKind::Endpoint,
        Some(endpoint.id.clone()),
    )
    .await
    .expect("select codex endpoint");

    let fixture = provider_probe_fixture(data_dir).await;
    let state = &fixture.daemon;
    let app = fixture.router();

    let (runtime_cmd, dep_bin_rel, handshake_marker) =
        setup_runtime_command_requiring_crp_handshake(fixture.data_dir.path(), "codex");
    seed_runtime_and_status(&state, "codex", runtime_cmd, dep_bin_rel).await;
    seed_managed_codex_cli_dependency(&state, "managed/runtime-node-codex/bin").await;

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (status, body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        format!("/api/workspaces/{}/providers/codex/verify", ws.id.0),
        Some(serde_json::json!({})),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "verify request failed: {body:#?}");
    assert_eq!(
        body.get("status").and_then(serde_json::Value::as_str),
        Some("ok"),
        "selected endpoint verify should use a CRP request handshake instead of an idle launch: {body:#?}"
    );
    assert!(
        handshake_marker.exists(),
        "selected endpoint verify returned ok without sending a CRP models.list handshake"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn provider_bootstrap_avoids_runtime_preparation_when_sandbox_runtime_preparation_is_unavailable(
) {
    let _env_lock = lock_env().await;
    let data_dir = tempfile::tempdir().expect("tempdir");
    #[cfg(target_os = "macos")]
    let _helper_env = {
        let helper = data_dir.path().join("ctx-avf-linux-helper");
        write_avf_probe_helper(&helper);
        EnvVarGuard::set(
            "CTX_AVF_LINUX_HELPER_PATH",
            helper.to_str().expect("helper path should be utf-8"),
        )
    };
    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    let fixture = provider_probe_fixture(data_dir).await;
    let state = &fixture.daemon;
    let app = fixture.router();

    upsert_amp_account(
        fixture.data_dir.path(),
        Some("Amp Test".to_string()),
        Some("amp@example.com".to_string()),
    )
    .await
    .expect("upsert amp account");
    let (bridge_cmd, bridge_dep_bin_rel) =
        setup_runtime_command_with_managed_interpreter(fixture.data_dir.path(), "acp-crp-bridge");
    seed_runtime_and_status(&state, "acp-crp-bridge", bridge_cmd, bridge_dep_bin_rel).await;
    let (runtime_cmd, dep_bin_rel) =
        setup_runtime_command_with_managed_interpreter(fixture.data_dir.path(), "amp");
    seed_runtime_and_status(&state, "amp", runtime_cmd, dep_bin_rel).await;

    let fake_sandbox_cli = fixture.data_dir.path().join("sandbox-cli");
    write_fake_sandbox_cli(&fake_sandbox_cli);
    let _sandbox_cli_guard = EnvVarGuard::set(
        "CTX_HARNESS_SANDBOX_CLI_PATH",
        fake_sandbox_cli
            .to_str()
            .expect("fake sandbox CLI path should be utf-8"),
    );

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (cfg_status, cfg_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        format!("/api/workspaces/{}/execution_config", ws.id.0),
        Some(serde_json::json!({
            "environment": "sandbox",
            "network_mode": "all",
        })),
    )
    .await;
    assert_eq!(
        cfg_status,
        StatusCode::OK,
        "execution config request failed: {cfg_body:#?}"
    );

    let (status, bootstrap): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        format!("/api/workspaces/{}/providers/bootstrap", ws.id.0),
        None,
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "bootstrap request failed: {bootstrap:#?}"
    );
    assert_eq!(
        bootstrap
            .pointer("/provider_options/amp/has_active_auth")
            .and_then(serde_json::Value::as_bool),
        Some(true),
        "bootstrap must hydrate auth from provider config/account state without runtime preparation: {bootstrap:#?}"
    );
    assert_eq!(
        bootstrap
            .pointer("/provider_options/amp/probe_ok")
            .and_then(serde_json::Value::as_bool),
        Some(false),
        "bootstrap should still reflect target-aware provider usability without touching sandbox runtime preparation: {bootstrap:#?}"
    );
    assert!(
        bootstrap
            .pointer("/provider_options/amp/probe_error")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|message| message.contains("does not verify target 'container'")),
        "bootstrap should surface provider-status target mismatch rather than a sandbox preparation failure: {bootstrap:#?}"
    );
    assert_ne!(
        bootstrap
            .pointer("/provider_options/amp/models/meta/catalog_source")
            .and_then(serde_json::Value::as_str),
        Some("runtime_probe_live"),
        "bootstrap must not promote sandbox runtime probe output into the bootstrap payload: {bootstrap:#?}"
    );

    let (status, options): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        format!("/api/workspaces/{}/providers/amp/options", ws.id.0),
        None,
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "options request failed: {options:#?}"
    );
    assert_eq!(
        options.get("probe_ok").and_then(serde_json::Value::as_bool),
        Some(false),
        "explicit runtime options probing must still fail when sandbox runtime preparation is unavailable: {options:#?}"
    );
    assert!(
        options
            .get("probe_error")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|message| {
                message.contains("does not verify target 'container'")
                    || message.contains("container runtime failed")
            }),
        "runtime options should still report runtime/container readiness problems: {options:#?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn provider_options_probe_uses_workspace_runtime_context_for_container_mode() {
    let _env_lock = lock_env().await;
    let data_dir = tempfile::tempdir().expect("tempdir");
    #[cfg(target_os = "macos")]
    let _helper_env = {
        let helper = data_dir.path().join("ctx-avf-linux-helper");
        write_avf_probe_helper(&helper);
        EnvVarGuard::set(
            "CTX_AVF_LINUX_HELPER_PATH",
            helper.to_str().expect("helper path should be utf-8"),
        )
    };
    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    let fixture = provider_probe_fixture(data_dir).await;
    let state = &fixture.daemon;
    let app = fixture.router();

    let fake_sandbox_cli = fixture.data_dir.path().join("sandbox-cli");
    write_fake_sandbox_cli(&fake_sandbox_cli);
    let _sandbox_cli_guard = EnvVarGuard::set(
        "CTX_HARNESS_SANDBOX_CLI_PATH",
        fake_sandbox_cli
            .to_str()
            .expect("fake sandbox CLI path should be utf-8"),
    );

    let (runtime_cmd, dep_bin_rel) =
        setup_runtime_command_with_managed_interpreter(fixture.data_dir.path(), "codex");
    seed_runtime_and_status(&state, "codex", runtime_cmd, dep_bin_rel).await;
    seed_managed_codex_cli_dependency(&state, "managed/runtime-node-codex/bin").await;

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (cfg_status, cfg_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        format!("/api/workspaces/{}/execution_config", ws.id.0),
        Some(serde_json::json!({
            "environment": "sandbox",
            "network_mode": "all",
        })),
    )
    .await;
    assert_eq!(
        cfg_status,
        StatusCode::OK,
        "execution config request failed: {cfg_body:#?}"
    );

    let (status, body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        format!("/api/workspaces/{}/providers/codex/options", ws.id.0),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "options request failed: {body:#?}");
    assert_eq!(
        body.get("probe_ok").and_then(serde_json::Value::as_bool),
        Some(false),
        "container-mode probe must use workspace runtime context; host probe success is invalid: {body:#?}"
    );
    let diagnostics = body
        .get("diagnostics")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        diagnostics.iter().any(|value| {
            let message = value.as_str().unwrap_or_default();
            message.contains("does not verify target 'container'")
                || message.contains("container runtime failed")
        }) || body
            .get("probe_error")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|message| message.contains("container runtime failed")),
        "expected container mode probe to surface target/runtime readiness diagnostics: {body:#?}"
    );
    assert!(
        body.get("probe_error")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|message| {
                message
                    == "provider is not ready until required dependencies are installed: codex-cli"
                    || message.contains("container runtime failed")
            }),
        "expected actionable probe_error for container mode: {body:#?}"
    );
    if body.get("usability").is_some() {
        assert_eq!(
            body.pointer("/usability/reason_code")
                .and_then(serde_json::Value::as_str),
            Some("missing_dependency"),
            "expected unusable provider status to stay actionable in container mode: {body:#?}"
        );
    }
}
