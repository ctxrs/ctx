#![cfg(unix)]

mod common;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::http::StatusCode;
use ctx_daemon::test_support::TestDaemon;
use ctx_managed_installs::{
    load_agent_server_config, save_agent_server_config, AgentServerCommand, AgentServerConfigFile,
    ManagedInstallMetadata,
};
use ctx_provider_install::install_state::{InstallId, InstallStateKind, InstallTarget};
use ctx_provider_matrix::{
    matrix_cache_path, ProviderArchiveKind, ProviderArchiveTarget, ProviderInstall, ProviderMatrix,
    ProviderMatrixEntry, ProviderMatrixEntryKind, ProviderRelease, ProviderReleaseStatus,
};
use ctx_providers::adapters::{ProviderHealth, ProviderStatus};
use sha2::{Digest, Sha256};

const TEST_CTX_EXACT_VERSION: &str = "0.59.0-canary.providerlifecycle";

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(value) = self.previous.take() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

async fn env_lock() -> tokio::sync::OwnedMutexGuard<()> {
    static LOCK: OnceLock<Arc<tokio::sync::Mutex<()>>> = OnceLock::new();
    LOCK.get_or_init(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
        .lock_owned()
        .await
}

async fn seed_provider_status(daemon: &TestDaemon, status: ProviderStatus) {
    let provider_id = status.provider_id.clone();
    daemon.upsert_provider_status(provider_id, status).await;
}

fn ensure_test_build_identity() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let identity_path = std::env::temp_dir()
            .join("ctx-provider-current-version-regressions-artifact-identity.json");
        let identity = format!(
            concat!(
                "{{\n",
                "  \"schemaVersion\": 1,\n",
                "  \"exactVersion\": \"{version}\",\n",
                "  \"buildId\": \"provider-current-version-regressions\",\n",
                "  \"compatibilityToken\": \"artifact-provider-current-version-regressions\"\n",
                "}}\n"
            ),
            version = TEST_CTX_EXACT_VERSION,
        );
        std::fs::write(&identity_path, identity).expect("write test build identity");
        std::env::set_var("CTX_BUILD_IDENTITY_PATH", &identity_path);
    });
}

fn file_url(path: &Path) -> String {
    url::Url::from_file_path(path)
        .expect("file url")
        .to_string()
}

fn write_executable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(path, contents).expect("write executable");
    let mut perms = std::fs::metadata(path).expect("metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("set permissions");
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("{digest:x}")
}

async fn save_matrix_fixture(data_root: &Path, matrix: &ProviderMatrix) {
    let path = matrix_cache_path(data_root);
    let parent = path.parent().expect("matrix cache parent");
    tokio::fs::create_dir_all(parent)
        .await
        .expect("create matrix cache dir");
    tokio::fs::write(
        &path,
        serde_json::to_vec_pretty(matrix).expect("serialize matrix"),
    )
    .await
    .expect("write matrix cache");
}

fn fixture_matrix_version() -> u32 {
    ctx_provider_matrix::builtin_matrix().version
}

fn managed_npm_status_entry(
    provider_id: &str,
    package: &str,
    installed_version: &str,
    latest_version: &str,
    context_min: &str,
) -> ProviderMatrixEntry {
    ProviderMatrixEntry {
        id: provider_id.to_string(),
        kind: ProviderMatrixEntryKind::Harness,
        display_name: Some(provider_id.to_string()),
        tier: Some("tier2".to_string()),
        command: None,
        managed_install: Some(ProviderInstall::Npm {
            package: package.to_string(),
            version: latest_version.to_string(),
            entrypoint: format!("node_modules/{package}/bin.js"),
            args: Vec::new(),
            targets: std::collections::HashMap::new(),
        }),
        provider_dependencies: Vec::new(),
        dependencies: Vec::new(),
        version_probe: None,
        releases: vec![
            ProviderRelease {
                version: installed_version.to_string(),
                status: ProviderReleaseStatus::Supported,
                upstream_version: Some(installed_version.to_string()),
                context_min: None,
                context_max: None,
                notes: None,
                provenance: None,
            },
            ProviderRelease {
                version: latest_version.to_string(),
                status: ProviderReleaseStatus::Supported,
                upstream_version: Some(latest_version.to_string()),
                context_min: Some(context_min.to_string()),
                context_max: None,
                notes: None,
                provenance: None,
            },
        ],
    }
}

fn managed_archive_host_entry(
    provider_id: &str,
    version: &str,
    url: String,
    sha256: String,
    bin_path: &str,
    context_min: &str,
) -> ProviderMatrixEntry {
    let host_target = ctx_managed_installs::resolve_matrix_target_key(InstallTarget::Host)
        .expect("host target key")
        .to_string();
    ProviderMatrixEntry {
        id: provider_id.to_string(),
        kind: ProviderMatrixEntryKind::Harness,
        display_name: Some(provider_id.to_string()),
        tier: Some("tier2".to_string()),
        command: None,
        managed_install: Some(ProviderInstall::Archive {
            version: version.to_string(),
            args: Vec::new(),
            targets: HashMap::from([(
                host_target,
                ProviderArchiveTarget {
                    url,
                    sha256: Some(sha256),
                    size_bytes: None,
                    archive: ProviderArchiveKind::None,
                    bin_path: bin_path.to_string(),
                },
            )]),
        }),
        provider_dependencies: Vec::new(),
        dependencies: Vec::new(),
        version_probe: None,
        releases: vec![ProviderRelease {
            version: version.to_string(),
            status: ProviderReleaseStatus::Supported,
            upstream_version: Some(version.to_string()),
            context_min: Some(context_min.to_string()),
            context_max: None,
            notes: None,
            provenance: None,
        }],
    }
}

fn managed_install_metadata(
    package: &str,
    version: &str,
    target: InstallTarget,
) -> ManagedInstallMetadata {
    ManagedInstallMetadata {
        package: Some(package.to_string()),
        version: Some(version.to_string()),
        artifact_fingerprint: Some(format!("npm:{package}@{version}")),
        archive_sha256: None,
        target: Some(target),
        install_dir_rel: Some(format!("providers/agent-servers/{package}/{version}")),
        bin_dir_rel: Some(format!("providers/agent-servers/{package}/{version}/bin")),
        last_success_at: None,
        last_error: None,
    }
}

async fn save_managed_provider_target(
    data_root: &Path,
    provider_id: &str,
    package: &str,
    version: &str,
    target: InstallTarget,
) {
    let mut cfg = load_agent_server_config(data_root)
        .await
        .unwrap_or_else(|_| AgentServerConfigFile::default());
    let meta = managed_install_metadata(package, version, target);
    cfg.managed_install_targets.insert(
        provider_id.to_string(),
        HashMap::from([(target.as_str().to_string(), meta.clone())]),
    );
    cfg.managed_provider_targets.insert(
        provider_id.to_string(),
        HashMap::from([(
            target.as_str().to_string(),
            AgentServerCommand {
                command: format!("/tmp/{provider_id}"),
                args: Vec::new(),
                dependencies: Vec::new(),
                managed: Some(meta),
            },
        )]),
    );
    save_agent_server_config(data_root, &cfg)
        .await
        .expect("save managed provider config");
}

async fn wait_for_install_completion(
    daemon: &TestDaemon,
    install_id: InstallId,
) -> ctx_provider_install::install_state::InstallInfo {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let info = daemon
            .get_install_info(install_id)
            .await
            .expect("missing install info");
        if !matches!(info.state, InstallStateKind::Running) {
            return info;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for install {install_id}: {info:#?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn container_provider_status_fails_closed_when_hybrid_artifact_is_missing() {
    let _env_lock = env_lock().await;
    ensure_test_build_identity();

    let data_dir = tempfile::tempdir().expect("tempdir");
    let matrix_path = matrix_cache_path(data_dir.path());
    save_matrix_fixture(
        data_dir.path(),
        &ProviderMatrix {
            version: fixture_matrix_version(),
            generated_at: None,
            providers: vec![managed_npm_status_entry(
                "gemini",
                "@google/gemini-cli",
                "0.33.1",
                "0.38.2",
                "0.59.0",
            )],
        },
    )
    .await;
    let _matrix_path_guard = EnvVarGuard::set(
        "CTX_BUNDLE_MATRIX_JSON",
        matrix_path.to_str().expect("matrix path utf-8"),
    );

    let fixture = common::fake_daemon_fixture_in_data_dir_with_providers(
        data_dir,
        HashMap::new(),
        "http://127.0.0.1:0",
    )
    .await;
    let daemon = &fixture.daemon;
    let app = fixture.router();

    seed_provider_status(
        daemon,
        ProviderStatus {
            provider_id: "gemini".to_string(),
            installed: false,
            detected_path: None,
            version: None,
            capabilities: None,
            health: ProviderHealth::Missing,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ctx_providers::adapters::ProviderUsability::default(),
        },
    )
    .await;

    let (status, body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        "/api/providers/gemini?target=container",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "provider route failed: {body:#?}");
    assert_eq!(
        body.pointer("/details/install_supported")
            .and_then(serde_json::Value::as_str),
        Some("false"),
        "container installs must fail closed when no staged artifact is published: {body:#?}"
    );
    assert_eq!(
        body.pointer("/details/install_blocked_code")
            .and_then(serde_json::Value::as_str),
        Some("container_artifact_missing"),
        "expected explicit container artifact failure instead of registry fallback: {body:#?}"
    );
    assert!(
        body.pointer("/details/install_blocked_reason")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|reason| reason.contains("published managed artifact")),
        "expected actionable missing-artifact detail: {body:#?}"
    );
}

#[tokio::test]
async fn host_provider_status_surfaces_stale_installs_for_current_ctx_build() {
    let _env_lock = env_lock().await;
    ensure_test_build_identity();

    let data_dir = tempfile::tempdir().expect("tempdir");
    let matrix_path = matrix_cache_path(data_dir.path());
    save_matrix_fixture(
        data_dir.path(),
        &ProviderMatrix {
            version: fixture_matrix_version(),
            generated_at: None,
            providers: vec![
                managed_npm_status_entry(
                    "codex",
                    "@openai/codex",
                    "0.124.0-ctx.1",
                    "1.0.0",
                    "0.59.0",
                ),
                managed_npm_status_entry(
                    "gemini",
                    "@google/gemini-cli",
                    "0.33.1",
                    "0.38.2",
                    "0.59.0",
                ),
                managed_npm_status_entry(
                    "cursor",
                    "@blowmage/cursor-agent-acp",
                    "0.7.1",
                    "0.7.1",
                    "0.59.0",
                ),
            ],
        },
    )
    .await;
    let _matrix_path_guard = EnvVarGuard::set(
        "CTX_BUNDLE_MATRIX_JSON",
        matrix_path.to_str().expect("matrix path utf-8"),
    );
    save_managed_provider_target(
        data_dir.path(),
        "codex",
        "@openai/codex",
        "0.124.0-ctx.1",
        InstallTarget::Host,
    )
    .await;
    save_managed_provider_target(
        data_dir.path(),
        "gemini",
        "@google/gemini-cli",
        "0.33.1",
        InstallTarget::Host,
    )
    .await;
    save_managed_provider_target(
        data_dir.path(),
        "cursor",
        "@blowmage/cursor-agent-acp",
        "0.7.1",
        InstallTarget::Host,
    )
    .await;

    let fixture = common::fake_daemon_fixture_in_data_dir_with_providers(
        data_dir,
        HashMap::new(),
        "http://127.0.0.1:0",
    )
    .await;
    let daemon = &fixture.daemon;
    let app = fixture.router();

    for (provider_id, version) in [
        ("codex", "0.124.0-ctx.1"),
        ("gemini", "0.33.1"),
        ("cursor", "0.7.1"),
    ] {
        seed_provider_status(
            daemon,
            ProviderStatus {
                provider_id: provider_id.to_string(),
                installed: true,
                detected_path: Some(format!("/tmp/{provider_id}")),
                version: Some(version.to_string()),
                capabilities: None,
                health: ProviderHealth::Ok,
                diagnostics: Vec::new(),
                details: HashMap::new(),
                usability: ctx_providers::adapters::ProviderUsability::default(),
            },
        )
        .await;
    }

    let (status, body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        "/api/providers?target=host",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "provider list failed: {body:#?}");

    let providers = body.as_array().expect("providers array");
    let find_provider = |provider_id: &str| {
        providers
            .iter()
            .find(|provider| {
                provider
                    .get("provider_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(provider_id)
            })
            .cloned()
            .unwrap_or_else(|| panic!("missing provider {provider_id} in {body:#?}"))
    };

    for (provider_id, expected_version) in [("codex", "1.0.0"), ("gemini", "0.38.2")] {
        let provider = find_provider(provider_id);
        assert_eq!(
            provider
                .pointer("/details/matrix_update_available")
                .and_then(serde_json::Value::as_str),
            Some("true"),
            "{provider_id} should be flagged as updateable: {provider:#?}"
        );
        assert_eq!(
            provider
                .pointer("/details/matrix_recommended_version")
                .and_then(serde_json::Value::as_str),
            Some(expected_version),
            "{provider_id} should expose the current-build recommendation: {provider:#?}"
        );
    }

    let codex = find_provider("codex");
    assert_eq!(
        codex
            .pointer("/details/install_supported")
            .and_then(serde_json::Value::as_str),
        Some("true"),
        "codex should remain installable for the current ctx build: {codex:#?}"
    );

    let cursor = find_provider("cursor");
    assert!(
        cursor.pointer("/details/matrix_update_available").is_none(),
        "current cursor install must not surface a stale update warning: {cursor:#?}"
    );
    assert_eq!(
        cursor.get("health").and_then(serde_json::Value::as_str),
        Some("ok"),
        "current cursor install should remain healthy: {cursor:#?}"
    );
}

#[tokio::test]
async fn managed_install_start_uses_runtime_build_identity_for_release_resolution() {
    let _env_lock = env_lock().await;
    ensure_test_build_identity();

    let data_dir = tempfile::tempdir().expect("tempdir");
    let fixture = data_dir.path().join("amp-acp");
    let fixture_contents = "#!/bin/sh\nexit 0\n";
    write_executable(&fixture, fixture_contents);
    let matrix_path = matrix_cache_path(data_dir.path());
    save_matrix_fixture(
        data_dir.path(),
        &ProviderMatrix {
            version: fixture_matrix_version(),
            generated_at: None,
            providers: vec![managed_archive_host_entry(
                "fixture-provider",
                "0.1.3",
                file_url(&fixture),
                sha256_hex(fixture_contents.as_bytes()),
                "fixture-provider",
                "0.59.0",
            )],
        },
    )
    .await;
    let _matrix_path_guard = EnvVarGuard::set(
        "CTX_BUNDLE_MATRIX_JSON",
        matrix_path.to_str().expect("matrix path utf-8"),
    );

    let fixture = common::fake_daemon_fixture_in_data_dir_with_providers(
        data_dir,
        HashMap::new(),
        "http://127.0.0.1:0",
    )
    .await;
    let daemon = &fixture.daemon;
    let app = fixture.router();

    let (install_status, install_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        "/api/providers/fixture-provider/install?target=host",
        None,
    )
    .await;
    assert_eq!(
        install_status,
        StatusCode::OK,
        "managed install should start successfully when the runtime build satisfies context_min: {install_body:#?}"
    );
    let install_id = install_body
        .get("install_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|raw| raw.parse::<InstallId>().ok())
        .expect("install id");
    let install_info = wait_for_install_completion(daemon, install_id).await;
    assert!(
        matches!(install_info.state, InstallStateKind::Succeeded),
        "managed install should succeed once started: {install_info:#?}"
    );

    let cfg = load_agent_server_config(fixture.data_dir.path())
        .await
        .expect("load managed provider config after install");
    let host_meta = cfg
        .managed_install_targets
        .get("fixture-provider")
        .and_then(|targets| targets.get(InstallTarget::Host.as_str()))
        .expect("fixture-provider host metadata");
    assert_eq!(
        host_meta.version.as_deref(),
        Some("0.1.3"),
        "successful managed install should persist the current-build-compatible release"
    );
}
