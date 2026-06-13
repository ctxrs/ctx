#![cfg(unix)]

mod common;

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;

use axum::http::StatusCode;
use ctx_daemon::test_support::TestDaemon;
use ctx_managed_installs::{
    resolve_matrix_target_key, save_agent_server_config, AgentServerCommand, AgentServerConfigFile,
    ManagedInstallMetadata,
};
use ctx_provider_install::install_state::InstallTarget;
use ctx_provider_matrix::{builtin_matrix, get_entry, recommended_release, ProviderInstall};
use ctx_provider_runtime::CachedProviderOptions;
use ctx_providers::adapters::{ProviderHealth, ProviderStatus};

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct EnvGuard {
    key: &'static str,
    prev: Option<String>,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prev }
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = self.prev.take() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn helper_env_test_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn write_runtime_fixture(path: &Path) {
    std::fs::write(path, "#!/bin/sh\nexit 0\n").expect("write runtime fixture");
}

async fn seed_provider_status(daemon: &TestDaemon, status: ProviderStatus) {
    let provider_id = status.provider_id.clone();
    daemon.upsert_provider_status(provider_id, status).await;
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn write_avf_probe_helper(path: &Path) {
    std::fs::write(
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
    )
    .expect("write avf probe helper");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)
            .expect("avf probe helper metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod avf probe helper");
    }
}

fn bridge_missing_status(provider_id: &str) -> ProviderStatus {
    ProviderStatus {
        provider_id: provider_id.to_string(),
        installed: false,
        detected_path: None,
        version: None,
        capabilities: None,
        health: ProviderHealth::Error,
        diagnostics: vec!["ACP bridge runtime is not configured or invalid".to_string()],
        details: HashMap::from([("error_code".to_string(), "acp_bridge_missing".to_string())]),
        usability: ctx_providers::adapters::ProviderUsability::default(),
    }
}

async fn seed_container_only_install(data_root: &Path, provider_id: &str) -> std::path::PathBuf {
    let install_dir = data_root
        .join("providers")
        .join("agent-servers")
        .join(provider_id)
        .join("fixture")
        .join("container");
    std::fs::create_dir_all(&install_dir).expect("create install dir");
    let command_path = install_dir.join(format!("{provider_id}-runtime"));
    write_runtime_fixture(&command_path);

    let matrix = builtin_matrix();
    let entry = get_entry(&matrix, provider_id).expect("provider matrix entry");
    let managed_install = entry
        .managed_install
        .as_ref()
        .expect("provider should be managed");
    let mut meta = ManagedInstallMetadata {
        package: None,
        version: None,
        archive_sha256: None,
        artifact_fingerprint: None,
        target: Some(InstallTarget::Container),
        install_dir_rel: Some(format!(
            "providers/agent-servers/{provider_id}/fixture/container"
        )),
        bin_dir_rel: Some(format!(
            "providers/agent-servers/{provider_id}/fixture/container"
        )),
        last_success_at: None,
        last_error: None,
    };
    match managed_install {
        ProviderInstall::Npm { package, .. } => {
            let version = recommended_release(entry, None)
                .expect("recommended release")
                .version
                .clone();
            meta.package = Some(package.clone());
            meta.version = Some(version.clone());
            meta.artifact_fingerprint = Some(format!("npm:{package}@{version}"));
        }
        ProviderInstall::Archive {
            version, targets, ..
        } => {
            let target_key =
                resolve_matrix_target_key(InstallTarget::Container).expect("target key");
            let target_entry = targets.get(target_key).expect("container archive target");
            meta.version = Some(version.clone());
            meta.archive_sha256 = target_entry.sha256.clone();
        }
        ProviderInstall::Python {
            package,
            version,
            python_version,
            python_build_tag,
            ..
        } => {
            meta.package = Some(package.clone());
            meta.version = Some(version.clone());
            let mut fingerprint = format!("python:{package}=={version}");
            if let Some(value) = python_version
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                fingerprint.push_str(&format!("|python={value}"));
            }
            if let Some(value) = python_build_tag
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                fingerprint.push_str(&format!("|build={value}"));
            }
            meta.artifact_fingerprint = Some(fingerprint);
        }
    }

    let mut cfg = AgentServerConfigFile::default();
    cfg.managed_install_targets.insert(
        provider_id.to_string(),
        HashMap::from([("container".to_string(), meta.clone())]),
    );
    cfg.managed_provider_targets.insert(
        provider_id.to_string(),
        HashMap::from([(
            "container".to_string(),
            AgentServerCommand {
                command: command_path.to_string_lossy().to_string(),
                args: Vec::new(),
                dependencies: Vec::new(),
                managed: Some(meta),
            },
        )]),
    );
    save_agent_server_config(data_root, &cfg)
        .await
        .expect("save agent server config");

    command_path
}

#[tokio::test]
async fn host_target_reports_target_mismatch_for_container_only_acp_installs() {
    let fixture =
        common::fake_daemon_fixture_with_providers(HashMap::new(), "http://127.0.0.1:0").await;
    let daemon = &fixture.daemon;
    let app = fixture.router();

    for provider_id in ["kimi", "mistral", "qwen"] {
        let _command_path = seed_container_only_install(fixture.data_dir.path(), provider_id).await;
        seed_provider_status(daemon, bridge_missing_status(provider_id)).await;

        let (status, body): (StatusCode, serde_json::Value) = common::json_request(
            &app,
            axum::http::Method::GET,
            format!("/api/providers/{provider_id}?target=host"),
            None,
        )
        .await;

        assert_eq!(
            status,
            StatusCode::OK,
            "provider route failed for {provider_id}: {body:#?}"
        );
        assert_eq!(
            body.get("installed").and_then(serde_json::Value::as_bool),
            Some(false),
            "expected host-target install=false for {provider_id}: {body:#?}"
        );
        assert_eq!(
            body.pointer("/details/target_mismatch")
                .and_then(serde_json::Value::as_str),
            Some("true"),
            "expected target_mismatch for {provider_id}: {body:#?}"
        );
        assert_eq!(
            body.pointer("/details/managed_target")
                .and_then(serde_json::Value::as_str),
            Some("container"),
            "expected managed_target=container for {provider_id}: {body:#?}"
        );
        let diagnostics = body
            .get("diagnostics")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(
            diagnostics.iter().any(|value| {
                value
                    .as_str()
                    .is_some_and(|text| text.contains("installed for target 'container'"))
            }),
            "expected target mismatch diagnostic for {provider_id}: {body:#?}"
        );
        assert_ne!(
            body.pointer("/details/error_code")
                .and_then(serde_json::Value::as_str),
            Some("acp_bridge_missing"),
            "host target should not surface ACP bridge missing when only container install exists: {body:#?}"
        );
    }
}

#[tokio::test]
async fn acp_provider_reports_missing_bridge_as_blocking_dependency() {
    let fixture =
        common::fake_daemon_fixture_with_providers(HashMap::new(), "http://127.0.0.1:0").await;
    let daemon = &fixture.daemon;
    let app = fixture.router();

    seed_provider_status(daemon, bridge_missing_status("qwen")).await;

    let (status, body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        "/api/providers/qwen?target=host",
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "provider route failed: {body:#?}");
    assert_eq!(
        body.pointer("/usability/usable")
            .and_then(serde_json::Value::as_bool),
        Some(false),
        "expected unusable ACP provider when bridge is missing: {body:#?}"
    );
    assert_eq!(
        body.pointer("/usability/status")
            .and_then(serde_json::Value::as_str),
        Some("blocked"),
        "expected blocked usability status when bridge is missing: {body:#?}"
    );
    assert_eq!(
        body.pointer("/usability/reason_code")
            .and_then(serde_json::Value::as_str),
        Some("missing_dependency"),
        "expected missing dependency reason when bridge is missing: {body:#?}"
    );
    assert_eq!(
        body.pointer("/usability/recommended_action")
            .and_then(serde_json::Value::as_str),
        Some("resolve_dependency"),
        "expected dependency resolution action when bridge is missing: {body:#?}"
    );
    let blocking_ids = body
        .pointer("/usability/blocking_provider_ids")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        blocking_ids
            .iter()
            .any(|value| value.as_str() == Some("acp-crp-bridge")),
        "expected blocking_provider_ids to include acp-crp-bridge: {body:#?}"
    );
    assert_eq!(
        body.pointer("/details/pending_dependency_ids")
            .and_then(serde_json::Value::as_str),
        Some("acp-crp-bridge"),
        "expected legacy pending_dependency_ids compatibility field: {body:#?}"
    );
}

#[tokio::test]
async fn workspace_options_use_workspace_target_status_for_acp_provider() {
    #[cfg(target_os = "macos")]
    let _helper_env_lock = helper_env_test_lock().lock().await;
    let data_dir = tempfile::tempdir().expect("tempdir");
    #[cfg(target_os = "macos")]
    let _helper_env = {
        let helper = data_dir.path().join("ctx-avf-linux-helper");
        write_avf_probe_helper(&helper);
        EnvGuard::set(
            "CTX_AVF_LINUX_HELPER_PATH",
            helper.to_string_lossy().as_ref(),
        )
    };
    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture_in_data_dir_with_providers(
        data_dir,
        HashMap::new(),
        "http://127.0.0.1:0",
    )
    .await;
    let daemon = &fixture.daemon;
    let app = fixture.router();

    let provider_id = "mistral";
    let _command_path = seed_container_only_install(fixture.data_dir.path(), provider_id).await;
    seed_provider_status(daemon, bridge_missing_status(provider_id)).await;

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (host_status, host_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        format!(
            "/api/workspaces/{}/providers/{provider_id}/options",
            ws.id.0
        ),
        None,
    )
    .await;

    assert_eq!(
        host_status,
        StatusCode::OK,
        "initial host-target options request failed: {host_body:#?}"
    );
    assert_eq!(
        host_body.get("installed").and_then(serde_json::Value::as_bool),
        Some(false),
        "host-target options should reflect unhealthy host status before switching targets: {host_body:#?}"
    );
    assert_eq!(
        host_body
            .get("probe_ok")
            .and_then(serde_json::Value::as_bool),
        Some(false),
        "host-target options should short-circuit unhealthy host state: {host_body:#?}"
    );

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
        "execution config failed: {cfg_body:#?}"
    );

    let cache_key_host = format!("{}/host/{provider_id}", ws.id.0);
    let cache_key_container = format!("{}/container/{provider_id}", ws.id.0);
    daemon
        .test_with_provider_options_cache(|cache| {
            cache.insert(
                cache_key_host,
                CachedProviderOptions {
                    cached_at: Instant::now(),
                    value: serde_json::json!({
                        "provider_id": provider_id,
                        "workspace_id": ws.id.0,
                        "installed": false,
                        "probe_ok": false,
                        "probe_error": "provider not installed or unhealthy",
                    }),
                },
            );
            cache.insert(
                cache_key_container,
                CachedProviderOptions {
                    cached_at: Instant::now(),
                    value: serde_json::json!({
                        "provider_id": provider_id,
                        "workspace_id": ws.id.0,
                        "installed": true,
                        "probe_ok": true,
                        "probe_error": serde_json::Value::Null,
                    }),
                },
            );
        })
        .await;

    let (status, body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        format!(
            "/api/workspaces/{}/providers/{provider_id}/options",
            ws.id.0
        ),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "options request failed: {body:#?}");
    assert_eq!(
        body.get("installed").and_then(serde_json::Value::as_bool),
        Some(true),
        "workspace options should use the container-target cache entry after switching targets: {body:#?}"
    );
    assert!(
        body.get("probe_error").and_then(serde_json::Value::as_str)
            != Some("provider not installed or unhealthy"),
        "workspace options should not reuse the stale host-target cache entry after switching targets: {body:#?}"
    );
}
