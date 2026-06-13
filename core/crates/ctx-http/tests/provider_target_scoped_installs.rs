#![cfg(unix)]

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ctx_daemon::test_support::TestDaemon;
use ctx_managed_installs::{
    agent_server_config_path, load_agent_server_config, save_agent_server_config,
    AgentServerCommand, AgentServerConfigFile, ManagedInstallMetadata,
};
use ctx_provider_install::install_state::{
    InstallErrorCode, InstallId, InstallInfo, InstallProgressEvent, InstallStateKind, InstallTarget,
};
use ctx_provider_matrix::{
    matrix_cache_path, ProviderArchiveKind, ProviderArchiveTarget, ProviderInstall,
    ProviderInstallDependency as MatrixProviderInstallDependency, ProviderInstallDependencyRole,
    ProviderInstallDependencyTarget, ProviderMatrix, ProviderMatrixEntry, ProviderMatrixEntryKind,
    ProviderRelease, ProviderReleaseStatus,
};
use ctx_provider_runtime::provider_launch::resolver::target_adapter_cache_key;
use ctx_providers::adapters::{ProviderAdapter, ProviderHealth, ProviderStatus};
use ctx_providers::crp::Tier1CrpAdapter;
use ctx_settings_model::{
    ContainerExecutionSettings, ContainerMountMode, ContainerNetworkMode, ExecutionMode,
    ExecutionSettings, Settings,
};
use sha2::{Digest, Sha256};

struct SeededRuntime {
    host_command: String,
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

fn provider_install_test_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
    }

    fn unset(key: &'static str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::remove_var(key);
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

fn clear_bundle_matrix_env() -> (EnvVarGuard, EnvVarGuard) {
    (
        EnvVarGuard::unset("CTX_BUNDLE_MATRIX_JSON"),
        EnvVarGuard::unset("CTX_BUNDLE_DIR"),
    )
}

fn write_executable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(path, contents).expect("write executable");
    let mut perms = std::fs::metadata(path).expect("metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("set permissions");
}

async fn write_invalid_agent_server_config(data_root: &Path) {
    let path = agent_server_config_path(data_root);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .expect("create agent server config parent");
    }
    tokio::fs::write(path, "{ invalid json")
        .await
        .expect("write invalid agent server config");
}

async fn seed_provider_status(daemon: &TestDaemon, status: ProviderStatus) {
    let provider_id = status.provider_id.clone();
    daemon.upsert_provider_status(provider_id, status).await;
}

async fn provider_install_fixture(
    data_root: &Path,
    providers: HashMap<String, Arc<dyn ProviderAdapter>>,
) -> common::ProviderInstallDaemonFixture {
    common::provider_install_daemon_fixture_for_data_root_with_providers(
        data_root,
        providers,
        "http://127.0.0.1:0",
    )
    .await
}

async fn providerless_install_fixture(data_root: &Path) -> common::ProviderInstallDaemonFixture {
    provider_install_fixture(data_root, HashMap::new()).await
}

async fn reopen_provider_install_fixture(
    data_root: &Path,
    providers: HashMap<String, Arc<dyn ProviderAdapter>>,
) -> common::ProviderInstallDaemonFixture {
    common::reopen_provider_install_daemon_fixture_for_data_root_with_providers(
        data_root,
        providers,
        "http://127.0.0.1:0",
    )
    .await
}

async fn reopen_providerless_install_fixture(
    data_root: &Path,
) -> common::ProviderInstallDaemonFixture {
    reopen_provider_install_fixture(data_root, HashMap::new()).await
}

fn write_fake_node_runtime(path: &Path, tag: &str) {
    let script = format!(
        r#"#!/bin/sh
set -eu
extract_field() {{
  printf '%s\n' "$1" | sed -n "s/.*\"$2\":\"\\([^\"]*\\)\".*/\\1/p"
}}
while IFS= read -r line; do
  case "$line" in
    *'"type":"models.list"'*)
      printf '{{"seq":1,"channel":"control","type":"models.list","models":[{{"id":"{tag}-model"}}],"current_model_id":"{tag}-model","catalog_source":"live_remote"}}\n'
      ;;
    *'"type":"session.open"'*)
      session_id="$(extract_field "$line" session_id)"
      if [ -z "$session_id" ]; then
        session_id="sess_{tag}"
      fi
      printf '{{"seq":2,"channel":"control","type":"session.opened","session_id":"%s","provider_session_id":"{tag}-provider"}}\n' "$session_id"
      ;;
    *'"type":"session.prompt"'*)
      session_id="$(extract_field "$line" session_id)"
      turn_id="$(extract_field "$line" turn_id)"
      if [ -z "$session_id" ]; then
        session_id="sess_{tag}"
      fi
      if [ -z "$turn_id" ]; then
        turn_id="turn_{tag}"
      fi
      printf '{{"seq":3,"channel":"control","type":"turn.started","session_id":"%s","turn_id":"%s"}}\n' "$session_id" "$turn_id"
      printf '{{"seq":4,"channel":"data","type":"message.final","session_id":"%s","turn_id":"%s","message_id":"msg_{tag}","content":"{tag}-runtime"}}\n' "$session_id" "$turn_id"
      printf '{{"seq":5,"channel":"control","type":"turn.completed","session_id":"%s","turn_id":"%s","status":"success"}}\n' "$session_id" "$turn_id"
      exit 0
      ;;
  esac
done
"#
    );
    write_executable(path, &script);
}

fn write_js_entrypoint(path: &Path) {
    write_executable(path, "#!/usr/bin/env node\n// fixture runtime\n");
}

const SEEDED_NODE_VERSION: &str = "24.15.0";
const SEEDED_NODE_DIST_TARGETS: &[(&str, &str)] = &[
    ("darwin-arm64", "host"),
    ("darwin-x64", "host"),
    ("linux-arm64", "container"),
    ("linux-x64", "container"),
    ("win-arm64", "container"),
    ("win-x64", "container"),
];

fn seeded_node_dist_target(target: InstallTarget) -> &'static str {
    match target {
        InstallTarget::Host => match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => "darwin-arm64",
            ("macos", "x86_64") => "darwin-x64",
            ("linux", "aarch64") => "linux-arm64",
            ("linux", "x86_64") => "linux-x64",
            ("windows", "aarch64") => "win-arm64",
            ("windows", "x86_64") => "win-x64",
            other => panic!("unsupported host platform for seeded node runtime: {other:?}"),
        },
        InstallTarget::Container => match std::env::consts::ARCH {
            "aarch64" => "linux-arm64",
            "x86_64" => "linux-x64",
            arch => panic!("unsupported container arch for seeded node runtime: {arch}"),
        },
        InstallTarget::LinuxAarch64 => "linux-arm64",
        InstallTarget::LinuxX8664 => "linux-x64",
    }
}

fn seeded_node_archive_sha256(dist_target: &str) -> &'static str {
    match dist_target {
        "darwin-arm64" => "372331b969779ab5d15b949884fc6eaf88d5afe87bde8ba881d6400b9100ffc4",
        "darwin-x64" => "ffd5ee293467927f3ee731a553eb88fd1f48cf74eebc2d74a6babe4af228673b",
        "linux-arm64" => "73afc234d558c24919875f51c2d1ea002a2ada4ea6f83601a383869fefa64eed",
        "linux-x64" => "44836872d9aec49f1e6b52a9a922872db9a2b02d235a616a5681b6a85fec8d89",
        "win-arm64" => "c9eb7402eda26e2ba7e44b6727fc85a8de56c5095b1f71ebd3062892211aa116",
        "win-x64" => "cc5149eabd53779ce1e7bdc5401643622d0c7e6800ade18928a767e940bb0e62",
        other => panic!("unsupported seeded node runtime target: {other}"),
    }
}

fn seeded_node_archive_name(dist_target: &str) -> String {
    let extension = if dist_target.starts_with("win-") {
        "zip"
    } else {
        "tar.gz"
    };
    format!("node-v{SEEDED_NODE_VERSION}-{dist_target}.{extension}")
}

fn seeded_node_install_folder(dist_target: &str) -> String {
    let sha = seeded_node_archive_sha256(dist_target);
    format!(
        "node-v{SEEDED_NODE_VERSION}-{dist_target}-sha256-{}",
        &sha[..12]
    )
}

fn seed_node_runtime_folder(data_root: &Path, folder: &str, tag: &str) {
    let node_root = data_root.join("runtimes").join("node").join(folder);
    let node_bin_dir = node_root.join("bin");
    let npm_cli = node_root
        .join("lib")
        .join("node_modules")
        .join("npm")
        .join("bin")
        .join("npm-cli.js");
    std::fs::create_dir_all(&node_bin_dir).expect("create node bin dir");
    std::fs::create_dir_all(
        npm_cli
            .parent()
            .expect("seeded npm cli should have a parent directory"),
    )
    .expect("create npm cli dir");
    write_fake_node_runtime(&node_bin_dir.join("node"), tag);
    write_js_entrypoint(&npm_cli);
}

fn seed_managed_node_runtime_folder(data_root: &Path, dist_target: &str, tag: &str) -> String {
    let folder = seeded_node_install_folder(dist_target);
    seed_node_runtime_folder(data_root, &folder, tag);

    let sha = seeded_node_archive_sha256(dist_target);
    let archive_name = seeded_node_archive_name(dist_target);
    let metadata = serde_json::json!({
        "schema_version": 1,
        "kind": "node",
        "version": SEEDED_NODE_VERSION,
        "target": dist_target,
        "archive_name": archive_name,
        "mirror_url": format!(
            "https://api.ctx.rs/storage/v1/object/public/releases/artifacts/managed-runtimes/node/{SEEDED_NODE_VERSION}/{archive_name}"
        ),
        "sha256": sha,
        "installed_at": "2026-04-30T00:00:00Z",
    });
    let metadata_path = data_root
        .join("runtimes")
        .join("node")
        .join(&folder)
        .join(".ctx-runtime-ready.json");
    std::fs::write(
        metadata_path,
        serde_json::to_vec_pretty(&metadata).expect("serialize seeded node runtime metadata"),
    )
    .expect("write seeded node runtime metadata");

    folder
}

fn seed_managed_node_runtime_metadata(cfg: &mut AgentServerConfigFile, data_root: &Path) {
    for (dist_target, tag) in SEEDED_NODE_DIST_TARGETS {
        seed_managed_node_runtime_folder(data_root, dist_target, tag);
    }

    for (dependency_id, target, tag) in [
        ("runtime-node-host", InstallTarget::Host, "host"),
        (
            "runtime-node-container",
            InstallTarget::Container,
            "container",
        ),
    ] {
        let dist_target = seeded_node_dist_target(target);
        let folder = seed_managed_node_runtime_folder(data_root, dist_target, tag);
        let sha = seeded_node_archive_sha256(dist_target);
        let node_root_rel = format!("runtimes/node/{folder}");
        let node_bin_rel = format!("{node_root_rel}/bin");

        cfg.managed_installs.insert(
            dependency_id.to_string(),
            ManagedInstallMetadata {
                package: Some("node-runtime".to_string()),
                version: Some(SEEDED_NODE_VERSION.to_string()),
                artifact_fingerprint: Some(format!(
                    "runtime:node:{SEEDED_NODE_VERSION}:sha256:{sha}"
                )),
                archive_sha256: Some(sha.to_string()),
                target: Some(target),
                install_dir_rel: Some(node_root_rel),
                bin_dir_rel: Some(node_bin_rel),
                last_success_at: None,
                last_error: None,
            },
        );
    }
}

async fn seed_target_scoped_codex_runtime(data_root: &Path) -> SeededRuntime {
    let host_install_rel = "providers/agent-servers/codex/host-fixture/bin/codex.js";
    let container_install_rel = "providers/agent-servers/codex/container-fixture/bin/codex.js";
    let host_command_path = data_root.join(host_install_rel);
    let container_command_path = data_root.join(container_install_rel);
    std::fs::create_dir_all(host_command_path.parent().expect("host parent"))
        .expect("create host runtime dir");
    std::fs::create_dir_all(container_command_path.parent().expect("container parent"))
        .expect("create container runtime dir");
    write_js_entrypoint(&host_command_path);
    write_js_entrypoint(&container_command_path);

    let mut cfg = AgentServerConfigFile::default();
    seed_managed_node_runtime_metadata(&mut cfg, data_root);
    cfg.managed_provider_targets.insert(
        "codex".to_string(),
        HashMap::from([
            (
                "host".to_string(),
                AgentServerCommand {
                    command: host_command_path.to_string_lossy().to_string(),
                    args: Vec::new(),
                    dependencies: vec!["runtime-node-host".to_string()],
                    managed: Some(ManagedInstallMetadata {
                        package: Some("@openai/codex".to_string()),
                        version: Some("1.0.0-host".to_string()),
                        artifact_fingerprint: None,
                        archive_sha256: None,
                        target: Some(ctx_provider_install::install_state::InstallTarget::Host),
                        install_dir_rel: Some(
                            "providers/agent-servers/codex/host-fixture".to_string(),
                        ),
                        bin_dir_rel: Some(
                            "providers/agent-servers/codex/host-fixture/bin".to_string(),
                        ),
                        last_success_at: None,
                        last_error: None,
                    }),
                },
            ),
            (
                "container".to_string(),
                AgentServerCommand {
                    command: container_command_path.to_string_lossy().to_string(),
                    args: Vec::new(),
                    dependencies: vec!["runtime-node-container".to_string()],
                    managed: Some(ManagedInstallMetadata {
                        package: Some("@openai/codex".to_string()),
                        version: Some("1.0.0-container".to_string()),
                        artifact_fingerprint: None,
                        archive_sha256: None,
                        target: Some(ctx_provider_install::install_state::InstallTarget::Container),
                        install_dir_rel: Some(
                            "providers/agent-servers/codex/container-fixture".to_string(),
                        ),
                        bin_dir_rel: Some(
                            "providers/agent-servers/codex/container-fixture/bin".to_string(),
                        ),
                        last_success_at: None,
                        last_error: None,
                    }),
                },
            ),
        ]),
    );
    cfg.managed_install_targets.insert(
        "codex".to_string(),
        HashMap::from([
            (
                "host".to_string(),
                ManagedInstallMetadata {
                    package: Some("@openai/codex".to_string()),
                    version: Some("1.0.0-host".to_string()),
                    artifact_fingerprint: None,
                    archive_sha256: None,
                    target: Some(ctx_provider_install::install_state::InstallTarget::Host),
                    install_dir_rel: Some("providers/agent-servers/codex/host-fixture".to_string()),
                    bin_dir_rel: Some("providers/agent-servers/codex/host-fixture/bin".to_string()),
                    last_success_at: None,
                    last_error: None,
                },
            ),
            (
                "container".to_string(),
                ManagedInstallMetadata {
                    package: Some("@openai/codex".to_string()),
                    version: Some("1.0.0-container".to_string()),
                    artifact_fingerprint: None,
                    archive_sha256: None,
                    target: Some(ctx_provider_install::install_state::InstallTarget::Container),
                    install_dir_rel: Some(
                        "providers/agent-servers/codex/container-fixture".to_string(),
                    ),
                    bin_dir_rel: Some(
                        "providers/agent-servers/codex/container-fixture/bin".to_string(),
                    ),
                    last_success_at: None,
                    last_error: None,
                },
            ),
        ]),
    );
    save_agent_server_config(data_root, &cfg)
        .await
        .expect("save agent config");

    SeededRuntime {
        host_command: host_command_path.to_string_lossy().to_string(),
    }
}

async fn build_state_with_host_codex(
    data_root: &Path,
    host_command: &str,
) -> common::ProviderInstallDaemonFixture {
    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert(
        "codex".to_string(),
        Arc::new(Tier1CrpAdapter::from_raw(
            "codex",
            host_command.to_string(),
            Vec::new(),
        )),
    );
    let fixture = common::provider_install_daemon_fixture_for_data_root_with_providers(
        data_root,
        providers,
        "http://127.0.0.1:0",
    )
    .await;
    fixture
        .daemon
        .refresh_provider_statuses()
        .await
        .expect("refresh provider statuses");
    fixture
}

async fn save_settings_to_data_root(data_root: &Path, settings: &Settings) {
    TestDaemon::preseed_settings_for_data_root_for_test(data_root, settings)
        .await
        .expect("preseed settings");
}

async fn configure_container_image_defaults(data_root: &Path) {
    let settings = Settings {
        execution: Some(ExecutionSettings {
            mode: ExecutionMode::Host,
            container: ContainerExecutionSettings {
                mount_mode: ContainerMountMode::DiskIsolated,
                network_mode: ContainerNetworkMode::All,
                allowlist: Vec::new(),
                image: Some("python:3.11".to_string()),
                ..Default::default()
            },
        }),
        ..Default::default()
    };
    save_settings_to_data_root(data_root, &settings).await;
}

async fn set_workspace_container_execution(
    app: &axum::Router,
    workspace_id: uuid::Uuid,
    environment: &str,
) {
    let req = Request::builder()
        .method(axum::http::Method::POST)
        .uri(format!("/api/workspaces/{workspace_id}/execution_config"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "environment": environment,
                "network_mode": "all",
            })
            .to_string(),
        ))
        .expect("build execution config request");
    let (status, body) = common::oneshot_bytes(app, req).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "execution config failed: {}",
        String::from_utf8_lossy(&body)
    );
}

async fn write_workspace_container_execution_without_runtime_probe(
    state: &TestDaemon,
    workspace_id: uuid::Uuid,
) {
    state
        .write_workspace_container_execution_without_runtime_probe_for_test(
            ctx_core::ids::WorkspaceId(workspace_id),
        )
        .await
        .expect("write workspace execution config");
}

async fn assert_target_adapter_not_cached(
    state: &TestDaemon,
    provider_id: &str,
    target: InstallTarget,
    context: &str,
) {
    let cache_key = target_adapter_cache_key(provider_id, target)
        .expect("non-host target should have a target adapter cache key");
    let cached = state
        .provider_target_has_adapter_cache_entry_for_test(&cache_key)
        .await;
    let keys = state.provider_target_adapter_cache_keys_for_test().await;
    assert!(
        !cached,
        "{context}: invalid managed config should not seed target adapter cache entry {cache_key}; keys={keys:?}"
    );
}

fn file_url(path: &Path) -> String {
    url::Url::from_file_path(path)
        .expect("file url")
        .to_string()
}

#[derive(Clone)]
struct DownloadFixture {
    body: Vec<u8>,
    delay_ms: u64,
}

struct DownloadFixtureServer {
    server: common::TestServer,
    sha256_by_name: HashMap<String, String>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

async fn spawn_download_fixture_server(
    fixtures: Vec<(&str, Vec<u8>, u64)>,
) -> DownloadFixtureServer {
    let mut sha256_by_name = HashMap::new();
    let fixture_map = Arc::new(
        fixtures
            .into_iter()
            .map(|(name, body, delay_ms)| {
                sha256_by_name.insert(name.to_string(), sha256_hex(&body));
                (name.to_string(), DownloadFixture { body, delay_ms })
            })
            .collect::<HashMap<_, _>>(),
    );

    async fn serve_fixture(
        axum::extract::State(fixtures): axum::extract::State<Arc<HashMap<String, DownloadFixture>>>,
        axum::extract::Path(name): axum::extract::Path<String>,
    ) -> impl axum::response::IntoResponse {
        let fixture = fixtures
            .get(&name)
            .cloned()
            .expect("missing download fixture");
        tokio::time::sleep(Duration::from_millis(fixture.delay_ms)).await;
        (
            [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
            fixture.body,
        )
    }

    let server = common::spawn_http_server(
        axum::Router::new()
            .route("/:name", axum::routing::get(serve_fixture))
            .with_state(fixture_map),
    )
    .await;
    DownloadFixtureServer {
        server,
        sha256_by_name,
    }
}

fn fixture_download_url(server: &DownloadFixtureServer, name: &str) -> String {
    let sha256 = server
        .sha256_by_name
        .get(name)
        .expect("download fixture should have sha256");
    format!("{}/{}?sha256={}", server.server.base_url, name, sha256)
}

fn fixture_matrix_version() -> u32 {
    ctx_provider_matrix::builtin_matrix().version
}

fn archive_sha256_for_url(url: &str) -> String {
    let parsed = url::Url::parse(url).expect("fixture archive url should parse");
    if let Some((_, sha256)) = parsed.query_pairs().find(|(key, _)| key == "sha256") {
        return sha256.into_owned();
    }
    if parsed.scheme() == "file" {
        let path = parsed
            .to_file_path()
            .expect("fixture file archive url should convert to path");
        return sha256_hex(&std::fs::read(path).expect("read fixture archive file"));
    }
    panic!("fixture archive url must be file:// or include sha256 query: {url}");
}

fn local_archive_entry_with_bin_path(url: String, bin_path: &str) -> ProviderArchiveTarget {
    let sha256 = archive_sha256_for_url(&url);
    ProviderArchiveTarget {
        url,
        sha256: Some(sha256),
        size_bytes: None,
        archive: ProviderArchiveKind::None,
        bin_path: bin_path.to_string(),
    }
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

struct MatrixFixtureEnv {
    _env_lock: tokio::sync::OwnedMutexGuard<()>,
    _bundle_dir: EnvVarGuard,
    _bundle_matrix: EnvVarGuard,
}

async fn env_lock() -> tokio::sync::OwnedMutexGuard<()> {
    static LOCK: OnceLock<Arc<tokio::sync::Mutex<()>>> = OnceLock::new();
    LOCK.get_or_init(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
        .lock_owned()
        .await
}

async fn activate_matrix_fixture(data_root: &Path, matrix: &ProviderMatrix) -> MatrixFixtureEnv {
    save_matrix_fixture(data_root, matrix).await;
    let env_lock = env_lock().await;
    let matrix_path = matrix_cache_path(data_root);
    MatrixFixtureEnv {
        _env_lock: env_lock,
        _bundle_dir: EnvVarGuard::unset("CTX_BUNDLE_DIR"),
        _bundle_matrix: EnvVarGuard::set(
            "CTX_BUNDLE_MATRIX_JSON",
            matrix_path.to_str().expect("matrix path utf-8"),
        ),
    }
}

fn archive_targets(url: String) -> HashMap<String, ProviderArchiveTarget> {
    archive_targets_with_bin_path(url, "bin/runtime")
}

fn archive_targets_with_bin_path(
    url: String,
    bin_path: &str,
) -> HashMap<String, ProviderArchiveTarget> {
    let mut targets = HashMap::from([
        (
            "linux-aarch64".to_string(),
            local_archive_entry_with_bin_path(url.clone(), bin_path),
        ),
        (
            "linux-x86_64".to_string(),
            local_archive_entry_with_bin_path(url.clone(), bin_path),
        ),
    ]);
    if let Ok(host_target_key) =
        ctx_managed_installs::resolve_matrix_target_key(InstallTarget::Host)
    {
        targets.insert(
            host_target_key.to_string(),
            local_archive_entry_with_bin_path(url, bin_path),
        );
    }
    targets
}

fn bridge_fixture_entry(bridge_url: String) -> ProviderMatrixEntry {
    let bridge_targets = archive_targets(bridge_url);
    ProviderMatrixEntry {
        id: "acp-crp-bridge".to_string(),
        kind: ProviderMatrixEntryKind::Dependency,
        display_name: Some("ACP Bridge".to_string()),
        tier: Some("tier2".to_string()),
        command: None,
        managed_install: Some(ProviderInstall::Archive {
            version: "0.1.0".to_string(),
            args: Vec::new(),
            targets: bridge_targets,
        }),
        provider_dependencies: Vec::new(),
        dependencies: Vec::new(),
        version_probe: None,
        releases: vec![ProviderRelease {
            version: "0.1.0".to_string(),
            status: ProviderReleaseStatus::Supported,
            upstream_version: None,
            provenance: None,
            context_min: None,
            context_max: None,
            notes: None,
        }],
    }
}

fn acp_provider_fixture_entry(provider_id: &str, provider_url: String) -> ProviderMatrixEntry {
    ProviderMatrixEntry {
        id: provider_id.to_string(),
        kind: ProviderMatrixEntryKind::Harness,
        display_name: Some(provider_id.to_string()),
        tier: Some("tier2".to_string()),
        command: None,
        managed_install: Some(ProviderInstall::Archive {
            version: "0.1.0".to_string(),
            args: vec!["--provider".to_string()],
            targets: archive_targets(provider_url),
        }),
        provider_dependencies: Vec::new(),
        dependencies: Vec::new(),
        version_probe: None,
        releases: vec![ProviderRelease {
            version: "0.1.0".to_string(),
            status: ProviderReleaseStatus::Supported,
            upstream_version: None,
            provenance: None,
            context_min: None,
            context_max: None,
            notes: None,
        }],
    }
}

fn archive_fixture_entry(
    provider_id: &str,
    kind: ProviderMatrixEntryKind,
    provider_url: String,
) -> ProviderMatrixEntry {
    ProviderMatrixEntry {
        id: provider_id.to_string(),
        kind,
        display_name: Some(provider_id.to_string()),
        tier: Some("tier2".to_string()),
        command: None,
        managed_install: Some(ProviderInstall::Archive {
            version: "0.1.0".to_string(),
            args: Vec::new(),
            targets: archive_targets(provider_url),
        }),
        provider_dependencies: Vec::new(),
        dependencies: Vec::new(),
        version_probe: None,
        releases: vec![ProviderRelease {
            version: "0.1.0".to_string(),
            status: ProviderReleaseStatus::Supported,
            upstream_version: None,
            provenance: None,
            context_min: None,
            context_max: None,
            notes: None,
        }],
    }
}

fn provider_fixture_matrix_with_providers(
    bridge_url: String,
    providers: Vec<(&str, String)>,
) -> ProviderMatrix {
    let mut entries = vec![bridge_fixture_entry(bridge_url)];
    entries.extend(
        providers.into_iter().map(|(provider_id, provider_url)| {
            acp_provider_fixture_entry(provider_id, provider_url)
        }),
    );
    ProviderMatrix {
        version: fixture_matrix_version(),
        generated_at: None,
        providers: entries,
    }
}

fn provider_fixture_matrix(bridge_url: String, provider_url: String) -> ProviderMatrix {
    provider_fixture_matrix_with_providers(bridge_url, vec![("kimi", provider_url)])
}

fn archive_js_harness_fixture_entry(
    provider_id: &str,
    version: &str,
    provider_url: String,
    bin_path: &str,
    upstream_version: Option<&str>,
) -> ProviderMatrixEntry {
    ProviderMatrixEntry {
        id: provider_id.to_string(),
        kind: ProviderMatrixEntryKind::Harness,
        display_name: Some(provider_id.to_string()),
        tier: Some("tier2".to_string()),
        command: None,
        managed_install: Some(ProviderInstall::Archive {
            version: version.to_string(),
            args: Vec::new(),
            targets: archive_targets_with_bin_path(provider_url, bin_path),
        }),
        provider_dependencies: Vec::new(),
        dependencies: Vec::new(),
        version_probe: None,
        releases: vec![ProviderRelease {
            version: version.to_string(),
            status: ProviderReleaseStatus::Supported,
            upstream_version: upstream_version.map(str::to_string),
            provenance: None,
            context_min: None,
            context_max: None,
            notes: None,
        }],
    }
}

fn npm_harness_with_archive_targets_fixture_entry(
    provider_id: &str,
    version: &str,
    provider_url: String,
    bin_path: &str,
) -> ProviderMatrixEntry {
    ProviderMatrixEntry {
        id: provider_id.to_string(),
        kind: ProviderMatrixEntryKind::Harness,
        display_name: Some(provider_id.to_string()),
        tier: Some("tier2".to_string()),
        command: None,
        managed_install: Some(ProviderInstall::Npm {
            package: format!("@ctx-fixture/{provider_id}"),
            version: version.to_string(),
            entrypoint: "node_modules/@ctx-fixture/provider/bin.js".to_string(),
            args: Vec::new(),
            targets: archive_targets_with_bin_path(provider_url, bin_path),
        }),
        provider_dependencies: Vec::new(),
        dependencies: Vec::new(),
        version_probe: None,
        releases: vec![ProviderRelease {
            version: version.to_string(),
            status: ProviderReleaseStatus::Supported,
            upstream_version: Some(version.to_string()),
            provenance: None,
            context_min: None,
            context_max: None,
            notes: None,
        }],
    }
}

async fn wait_for_install_completion(
    state: &TestDaemon,
    install_id: InstallId,
) -> ctx_provider_install::install_state::InstallInfo {
    wait_for_install_completion_with_timeout(state, install_id, Duration::from_secs(60)).await
}

async fn wait_for_install_completion_with_timeout(
    state: &TestDaemon,
    install_id: InstallId,
    timeout: Duration,
) -> ctx_provider_install::install_state::InstallInfo {
    state
        .wait_for_provider_target_install_completion_for_test(install_id, timeout)
        .await
        .expect("wait for install completion")
}

fn parse_install_ids(body: &serde_json::Value) -> HashMap<String, InstallId> {
    body.as_array()
        .cloned()
        .expect("install response should be an array")
        .into_iter()
        .map(|entry| {
            let provider_id = entry
                .get("provider_id")
                .and_then(serde_json::Value::as_str)
                .expect("provider id")
                .to_string();
            let install_id = entry
                .get("install_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|raw| raw.parse::<InstallId>().ok())
                .expect("install id");
            (provider_id, install_id)
        })
        .collect()
}

fn install_stage_progress_value(stage: &str) -> Option<u32> {
    match stage {
        "start" => Some(2),
        "prerequisites" => Some(2),
        "download" => Some(10),
        "node" => Some(15),
        "node_download" => Some(18),
        "prepare" => Some(25),
        "venv" => Some(35),
        "npm_install" => Some(65),
        "pip_install" => Some(70),
        "extract" => Some(78),
        "entrypoint" => Some(80),
        "inspect" => Some(90),
        "refresh" => Some(95),
        "registry" => Some(98),
        _ => None,
    }
}

fn compute_polled_install_pct(info: &InstallInfo, previous_pct: Option<u32>) -> Option<u32> {
    if matches!(info.state, InstallStateKind::Succeeded) {
        return Some(100);
    }
    let last_event = info.last_event.as_ref()?;
    let staged = install_stage_progress_value(last_event.stage.as_str())?;
    Some(previous_pct.map_or(staged, |pct| pct.max(staged)))
}

async fn get_install_info_api(app: &axum::Router, install_id: InstallId) -> InstallInfo {
    let (status, body): (StatusCode, InstallInfo) = common::json_request(
        app,
        axum::http::Method::GET,
        format!("/api/providers/install/{install_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "install info failed: {body:#?}");
    body
}

async fn wait_for_running_install_progress(
    state: &TestDaemon,
    install_id: InstallId,
) -> ctx_provider_install::install_state::InstallInfo {
    state
        .wait_for_provider_target_running_install_progress_for_test(
            install_id,
            Duration::from_secs(5),
        )
        .await
        .expect("wait for running install progress")
}

async fn wait_for_running_install_id(
    state: &TestDaemon,
    provider_id: &str,
    target: Option<InstallTarget>,
) -> InstallId {
    state
        .wait_for_provider_target_running_install_id_for_test(
            provider_id,
            target,
            Duration::from_secs(10),
        )
        .await
        .expect("wait for running install id")
}

async fn wait_for_tracked_install_id(
    state: &TestDaemon,
    provider_id: &str,
    target: Option<InstallTarget>,
) -> InstallId {
    state
        .wait_for_provider_target_tracked_install_id_for_test(
            provider_id,
            target,
            Duration::from_secs(10),
        )
        .await
        .expect("wait for tracked install id")
}

async fn wait_for_prerequisite_visibility(
    state: &TestDaemon,
    app: &axum::Router,
    install_id: InstallId,
    prerequisite_install_id: InstallId,
) -> InstallInfo {
    let prerequisite_install_id_string = prerequisite_install_id.to_string();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let prerequisite_info = state
            .provider_target_install_info_for_test(prerequisite_install_id)
            .await
            .expect("missing prerequisite install info");
        let info = get_install_info_api(app, install_id).await;
        if matches!(prerequisite_info.state, InstallStateKind::Running)
            && prerequisite_info.last_event.is_some()
            && info.last_event.as_ref().is_some_and(|event| {
                event.message.contains("acp-crp-bridge")
                    && event.message.contains(&prerequisite_install_id_string)
            })
        {
            return info;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for live prerequisite visibility on install {install_id}: prerequisite={prerequisite_info:#?} parent={info:#?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn get_install_events_api(
    app: &axum::Router,
    install_id: InstallId,
) -> Vec<InstallProgressEvent> {
    let (status, body): (StatusCode, Vec<InstallProgressEvent>) = common::json_request(
        app,
        axum::http::Method::GET,
        format!("/api/providers/install/{install_id}/events"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "install events failed: {body:#?}");
    body
}

async fn save_invalid_container_bridge_runtime(data_root: &Path) {
    let mut cfg = load_agent_server_config(data_root)
        .await
        .unwrap_or_default();
    cfg.managed_provider_targets.insert(
        "acp-crp-bridge".to_string(),
        HashMap::from([(
            "container".to_string(),
            AgentServerCommand {
                command: "relative-bridge".to_string(),
                args: Vec::new(),
                dependencies: Vec::new(),
                managed: Some(ManagedInstallMetadata {
                    package: Some("acp-crp-bridge".to_string()),
                    version: Some("1.0.0".to_string()),
                    artifact_fingerprint: None,
                    archive_sha256: None,
                    target: Some(ctx_provider_install::install_state::InstallTarget::Container),
                    install_dir_rel: Some(
                        "providers/agent-servers/acp-crp-bridge/invalid".to_string(),
                    ),
                    bin_dir_rel: Some("providers/agent-servers/acp-crp-bridge/invalid".to_string()),
                    last_success_at: None,
                    last_error: None,
                }),
            },
        )]),
    );
    save_agent_server_config(data_root, &cfg)
        .await
        .expect("save invalid bridge runtime config");
}

async fn post_message(app: &axum::Router, session_id: uuid::Uuid, content: &str) {
    let (status, body): (StatusCode, serde_json::Value) = common::json_request(
        app,
        axum::http::Method::POST,
        format!("/api/sessions/{session_id}/messages"),
        Some(serde_json::json!({ "content": content })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "message post failed: {body:#?}");
}

async fn wait_for_done_with_assistant_message(
    state: &TestDaemon,
    session_id: ctx_core::ids::SessionId,
    expected: &str,
) {
    state
        .provider_target_session_events_after_done_for_test(
            session_id,
            expected,
            Duration::from_secs(90),
        )
        .await
        .expect("wait for done event and assistant message");
}

fn sandbox_cli_binary_for_tests() -> Option<PathBuf> {
    if let Ok(raw) = std::env::var("CTX_HARNESS_SANDBOX_CLI_PATH") {
        let path = PathBuf::from(raw);
        if path.exists() {
            return Some(path);
        }
    }
    which::which("nerdctl").ok()
}

async fn sandbox_cli_ready(sandbox_cli: &Path) -> bool {
    tokio::process::Command::new(sandbox_cli)
        .arg("version")
        .output()
        .await
        .ok()
        .is_some_and(|output| output.status.success())
}

async fn sandbox_cli_has_image(sandbox_cli: &Path, image: &str) -> bool {
    tokio::process::Command::new(sandbox_cli)
        .args(["image", "exists", image])
        .output()
        .await
        .ok()
        .is_some_and(|output| output.status.success())
}

#[tokio::test]
async fn provider_status_http_keeps_host_and_container_installs_independent() {
    let _install_lock = provider_install_test_lock().lock().await;
    let _bundle_env = clear_bundle_matrix_env();
    let data_dir = tempfile::tempdir().expect("tempdir");
    let runtime = seed_target_scoped_codex_runtime(data_dir.path()).await;
    configure_container_image_defaults(data_dir.path()).await;
    let fixture = build_state_with_host_codex(data_dir.path(), &runtime.host_command).await;
    let app = fixture.router();

    let repo = common::init_git_repo(&[("note.txt", "hello\n")]).await;
    common::create_workspace(&app, repo.path(), "host-ws").await;

    let (host_status, host_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        "/api/providers/codex?target=host",
        None,
    )
    .await;
    assert_eq!(
        host_status,
        StatusCode::OK,
        "host provider failed: {host_body:#?}"
    );
    assert_eq!(
        host_body
            .get("installed")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        host_body
            .pointer("/details/managed_target")
            .and_then(serde_json::Value::as_str),
        Some("host")
    );
    assert_eq!(
        host_body
            .pointer("/details/managed_version")
            .and_then(serde_json::Value::as_str),
        Some("1.0.0-host")
    );

    let (container_status, container_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        "/api/providers/codex?target=container",
        None,
    )
    .await;
    assert_eq!(
        container_status,
        StatusCode::OK,
        "container provider failed: {container_body:#?}"
    );
    assert_eq!(
        container_body
            .get("installed")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        container_body
            .pointer("/details/managed_target")
            .and_then(serde_json::Value::as_str),
        Some("container")
    );
    assert_eq!(
        container_body
            .pointer("/details/managed_version")
            .and_then(serde_json::Value::as_str),
        Some("1.0.0-container")
    );
}

#[tokio::test]
async fn host_hybrid_npm_provider_uses_published_archive_target_when_available() {
    let _install_lock = provider_install_test_lock().lock().await;
    let _bundle_env = clear_bundle_matrix_env();
    let data_dir = tempfile::tempdir().expect("tempdir");
    let fixture_dir = data_dir.path().join("fixtures");
    std::fs::create_dir_all(&fixture_dir).expect("create fixture dir");
    let bridge_fixture = fixture_dir.join("acp-crp-bridge");
    let provider_fixture = fixture_dir.join("fixture-npm-acp");
    write_executable(&bridge_fixture, "#!/bin/sh\nexit 0\n");
    write_executable(&provider_fixture, "#!/bin/sh\nexit 0\n");
    let provider_url = file_url(&provider_fixture);
    let _matrix_fixture = activate_matrix_fixture(
        data_dir.path(),
        &ProviderMatrix {
            version: fixture_matrix_version(),
            generated_at: None,
            providers: vec![
                npm_harness_with_archive_targets_fixture_entry(
                    "fixture-npm",
                    "0.38.2",
                    provider_url.clone(),
                    "fixture-npm-acp",
                ),
                bridge_fixture_entry(file_url(&bridge_fixture)),
            ],
        },
    )
    .await;

    let fixture = providerless_install_fixture(data_dir.path()).await;
    let state = fixture.daemon.clone();
    let app = fixture.router();

    let (install_status, install_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        "/api/providers/fixture-npm/install?target=host",
        None,
    )
    .await;
    assert_eq!(
        install_status,
        StatusCode::OK,
        "host install should start successfully: {install_body:#?}"
    );
    let install_id = install_body
        .get("install_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|raw| raw.parse::<InstallId>().ok())
        .expect("install id");

    let install_info = wait_for_install_completion(&state, install_id).await;
    assert!(
        matches!(install_info.state, InstallStateKind::Succeeded),
        "host npm provider should install from its archive target without invoking live npm: {install_info:#?}"
    );

    let cfg = load_agent_server_config(data_dir.path())
        .await
        .expect("load agent server config");
    let host_meta = cfg
        .managed_install_targets
        .get("fixture-npm")
        .and_then(|targets| targets.get("host"))
        .expect("fixture-npm host install metadata should be recorded");
    assert_eq!(
        host_meta.package.as_deref(),
        Some(provider_url.as_str()),
        "host npm provider should record the archive URL, not the npm package name"
    );
    assert_eq!(
        host_meta.target,
        Some(InstallTarget::Host),
        "host install metadata should remain target-scoped"
    );
    let host_command = cfg
        .managed_provider_targets
        .get("fixture-npm")
        .and_then(|targets| targets.get("host"))
        .expect("fixture-npm host runtime command should be recorded");
    assert!(
        host_command.command.ends_with("fixture-npm-acp"),
        "archive-backed host runtime should point at the extracted provider binary: {host_command:#?}"
    );
}

#[tokio::test]
async fn acp_container_install_surfaces_bridge_as_installable_prerequisite() {
    let _install_lock = provider_install_test_lock().lock().await;
    let _bundle_env = clear_bundle_matrix_env();
    let data_dir = tempfile::tempdir().expect("tempdir");
    let fixture_dir = data_dir.path().join("fixtures");
    std::fs::create_dir_all(&fixture_dir).expect("create fixture dir");
    let bridge_fixture = fixture_dir.join("acp-crp-bridge");
    let provider_fixture = fixture_dir.join("kimi-acp");
    write_executable(&bridge_fixture, "#!/bin/sh\nexit 0\n");
    write_executable(&provider_fixture, "#!/bin/sh\nexit 0\n");
    let _matrix_fixture = activate_matrix_fixture(
        data_dir.path(),
        &provider_fixture_matrix(file_url(&bridge_fixture), file_url(&provider_fixture)),
    )
    .await;
    let fixture = providerless_install_fixture(data_dir.path()).await;
    let state = fixture.daemon.clone();
    let app = fixture.router();

    seed_provider_status(
        &state,
        ProviderStatus {
            provider_id: "kimi".to_string(),
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

    let (provider_status, provider_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        "/api/providers/kimi?target=container",
        None,
    )
    .await;
    assert_eq!(
        provider_status,
        StatusCode::OK,
        "provider status failed: {provider_body:#?}"
    );
    assert_eq!(
        provider_body
            .pointer("/details/install_supported")
            .and_then(serde_json::Value::as_str),
        Some("true"),
        "container ACP installs should remain supported when the bridge is installable: {provider_body:#?}"
    );
    assert!(
        provider_body
            .pointer("/details/install_blocked_code")
            .is_none(),
        "installable bridge prerequisites must not be surfaced as blocked: {provider_body:#?}"
    );

    let (providers_status, providers_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        "/api/providers?target=container",
        None,
    )
    .await;
    assert_eq!(
        providers_status,
        StatusCode::OK,
        "providers list failed: {providers_body:#?}"
    );
    let kimi_status = providers_body
        .as_array()
        .and_then(|providers| {
            providers.iter().find(|provider| {
                provider
                    .get("provider_id")
                    .and_then(serde_json::Value::as_str)
                    == Some("kimi")
            })
        })
        .cloned()
        .expect("kimi must appear in provider list");
    assert_eq!(
        kimi_status
            .pointer("/details/install_supported")
            .and_then(serde_json::Value::as_str),
        Some("true"),
        "providers list should advertise installable ACP container targets: {kimi_status:#?}"
    );
    assert!(
        kimi_status.pointer("/details/install_blocked_code").is_none(),
        "providers list must not mark installable ACP bridge prerequisites as blocked: {kimi_status:#?}"
    );
}

#[tokio::test]
async fn acp_host_install_surfaces_bridge_as_installable_prerequisite() {
    let _install_lock = provider_install_test_lock().lock().await;
    let _bundle_env = clear_bundle_matrix_env();
    let data_dir = tempfile::tempdir().expect("tempdir");
    let fixture_dir = data_dir.path().join("fixtures");
    std::fs::create_dir_all(&fixture_dir).expect("create fixture dir");
    let bridge_fixture = fixture_dir.join("acp-crp-bridge");
    let provider_fixture = fixture_dir.join("kimi-acp");
    write_executable(&bridge_fixture, "#!/bin/sh\nexit 0\n");
    write_executable(&provider_fixture, "#!/bin/sh\nexit 0\n");
    let _matrix_fixture = activate_matrix_fixture(
        data_dir.path(),
        &provider_fixture_matrix(file_url(&bridge_fixture), file_url(&provider_fixture)),
    )
    .await;
    let fixture = providerless_install_fixture(data_dir.path()).await;
    let state = fixture.daemon.clone();
    let app = fixture.router();

    seed_provider_status(
        &state,
        ProviderStatus {
            provider_id: "kimi".to_string(),
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

    let (provider_status, provider_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        "/api/providers/kimi?target=host",
        None,
    )
    .await;
    assert_eq!(
        provider_status,
        StatusCode::OK,
        "provider status failed: {provider_body:#?}"
    );
    assert_eq!(
        provider_body
            .pointer("/details/install_supported")
            .and_then(serde_json::Value::as_str),
        Some("true"),
        "host ACP installs should remain supported when the bridge is installable: {provider_body:#?}"
    );
    assert!(
        provider_body
            .pointer("/details/install_blocked_code")
            .is_none(),
        "installable bridge prerequisites must not be surfaced as blocked on host: {provider_body:#?}"
    );

    let (providers_status, providers_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        "/api/providers?target=host",
        None,
    )
    .await;
    assert_eq!(
        providers_status,
        StatusCode::OK,
        "providers list failed: {providers_body:#?}"
    );
    let kimi_status = providers_body
        .as_array()
        .and_then(|providers| {
            providers.iter().find(|provider| {
                provider
                    .get("provider_id")
                    .and_then(serde_json::Value::as_str)
                    == Some("kimi")
            })
        })
        .cloned()
        .expect("kimi must appear in provider list");
    assert_eq!(
        kimi_status
            .pointer("/details/install_supported")
            .and_then(serde_json::Value::as_str),
        Some("true"),
        "providers list should advertise installable ACP host targets: {kimi_status:#?}"
    );
    assert!(
        kimi_status.pointer("/details/install_blocked_code").is_none(),
        "providers list must not mark installable ACP bridge prerequisites as blocked on host: {kimi_status:#?}"
    );
}

#[tokio::test]
async fn acp_container_install_keeps_invalid_bridge_runtime_repairable_before_start() {
    let _install_lock = provider_install_test_lock().lock().await;
    let _bundle_env = clear_bundle_matrix_env();
    let data_dir = tempfile::tempdir().expect("tempdir");
    let fixture_dir = data_dir.path().join("fixtures");
    std::fs::create_dir_all(&fixture_dir).expect("create fixture dir");
    let bridge_fixture = fixture_dir.join("acp-crp-bridge");
    let provider_fixture = fixture_dir.join("kimi-acp");
    write_executable(&bridge_fixture, "#!/bin/sh\nexit 0\n");
    write_executable(&provider_fixture, "#!/bin/sh\nexit 0\n");
    let _matrix_fixture = activate_matrix_fixture(
        data_dir.path(),
        &provider_fixture_matrix(file_url(&bridge_fixture), file_url(&provider_fixture)),
    )
    .await;
    let fixture = providerless_install_fixture(data_dir.path()).await;
    let state = fixture.daemon.clone();
    let app = fixture.router();

    save_invalid_container_bridge_runtime(data_dir.path()).await;

    seed_provider_status(
        &state,
        ProviderStatus {
            provider_id: "kimi".to_string(),
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

    let (provider_status, provider_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        "/api/providers/kimi?target=container",
        None,
    )
    .await;
    assert_eq!(
        provider_status,
        StatusCode::OK,
        "provider status failed: {provider_body:#?}"
    );
    assert_eq!(
        provider_body
            .pointer("/details/install_supported")
            .and_then(serde_json::Value::as_str),
        Some("true"),
        "stale invalid managed bridge runtime should remain repairable for ACP installs: {provider_body:#?}"
    );
    assert_eq!(
        provider_body
            .pointer("/usability/status")
            .and_then(serde_json::Value::as_str),
        Some("blocked"),
        "repairable bridge prerequisites should keep ACP installs actionable instead of unsupported: {provider_body:#?}"
    );
    assert_eq!(
        provider_body
            .pointer("/usability/reason_code")
            .and_then(serde_json::Value::as_str),
        Some("missing_dependency"),
        "repairable bridge prerequisites should surface the canonical missing-dependency reason: {provider_body:#?}"
    );
    assert_eq!(
        provider_body
            .pointer("/details/pending_dependency_ids")
            .and_then(serde_json::Value::as_str),
        Some("acp-crp-bridge"),
        "repairable bridge prerequisites should remain listed as pending dependencies: {provider_body:#?}"
    );
    assert!(
        provider_body
            .pointer("/details/install_blocked_code")
            .is_none(),
        "repairable bridge prerequisites must not be surfaced as blocked installs: {provider_body:#?}"
    );
}

#[tokio::test]
async fn provider_target_scoped_installs_install_all_repairs_invalid_bridge_and_keeps_acp_dependents_installable(
) {
    let _install_lock = provider_install_test_lock().lock().await;
    let _bundle_env = clear_bundle_matrix_env();
    let data_dir = tempfile::tempdir().expect("tempdir");
    let fixture_dir = data_dir.path().join("fixtures");
    std::fs::create_dir_all(&fixture_dir).expect("create fixture dir");
    let bridge_fixture = fixture_dir.join("acp-crp-bridge");
    let kimi_fixture = fixture_dir.join("kimi-acp");
    let qwen_fixture = fixture_dir.join("qwen-acp");
    write_executable(&bridge_fixture, "#!/bin/sh\nsleep 0.3\nexit 0\n");
    write_executable(&kimi_fixture, "#!/bin/sh\nexit 0\n");
    write_executable(&qwen_fixture, "#!/bin/sh\nexit 0\n");
    let _matrix_fixture = activate_matrix_fixture(
        data_dir.path(),
        &provider_fixture_matrix_with_providers(
            file_url(&bridge_fixture),
            vec![
                ("kimi", file_url(&kimi_fixture)),
                ("qwen", file_url(&qwen_fixture)),
            ],
        ),
    )
    .await;

    save_invalid_container_bridge_runtime(data_dir.path()).await;

    let fixture = providerless_install_fixture(data_dir.path()).await;
    let state = fixture.daemon.clone();
    let app = fixture.router();

    let (install_status, install_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        "/api/providers/install_all?target=container",
        None,
    )
    .await;
    assert_eq!(
        install_status,
        StatusCode::OK,
        "bulk install should accept the bridge repair flow: {install_body:#?}"
    );
    let install_ids = parse_install_ids(&install_body);
    assert_eq!(
        install_ids.len(),
        3,
        "bulk install should return the bridge repair plus the selectable ACP harness installs: {install_body:#?}"
    );
    assert!(
        install_ids.contains_key("acp-crp-bridge"),
        "bridge repair should stay visible to bulk install callers: {install_body:#?}"
    );
    assert!(
        install_ids.contains_key("kimi"),
        "kimi should remain installable through the repaired dependency path: {install_body:#?}"
    );
    assert!(
        install_ids.contains_key("qwen"),
        "qwen should remain installable through the repaired dependency path: {install_body:#?}"
    );
    let bridge_install_id = *install_ids
        .get("acp-crp-bridge")
        .expect("missing bridge install id");

    let bridge_install_info = wait_for_install_completion(&state, bridge_install_id).await;
    assert!(
        matches!(bridge_install_info.state, InstallStateKind::Succeeded),
        "bridge repair should succeed during bulk install: {bridge_install_info:#?}"
    );
    for provider_id in ["kimi", "qwen"] {
        let install_info = wait_for_install_completion(
            &state,
            *install_ids
                .get(provider_id)
                .expect("missing install id from bulk response"),
        )
        .await;
        assert!(
            matches!(install_info.state, InstallStateKind::Succeeded),
            "{provider_id} should succeed after the bridge repair batch: {install_info:#?}"
        );
    }

    let reloaded_fixture = reopen_providerless_install_fixture(data_dir.path()).await;
    let reloaded_app = reloaded_fixture.router();

    for provider_id in ["kimi", "qwen"] {
        let (provider_status, provider_body): (StatusCode, serde_json::Value) =
            common::json_request(
                &reloaded_app,
                axum::http::Method::GET,
                format!("/api/providers/{provider_id}?target=container"),
                None,
            )
            .await;
        assert_eq!(
            provider_status,
            StatusCode::OK,
            "provider status failed after bulk repair/install: {provider_body:#?}"
        );
        assert_eq!(
            provider_body
                .get("installed")
                .and_then(serde_json::Value::as_bool),
            Some(true),
            "{provider_id} should be installed by the same bulk repair batch: {provider_body:#?}"
        );
    }
}

#[tokio::test]
async fn provider_target_scoped_installs_install_all_container_js_archive_harnesses_stay_current_after_success(
) {
    let _install_lock = provider_install_test_lock().lock().await;
    let _bundle_env = clear_bundle_matrix_env();
    let data_dir = tempfile::tempdir().expect("tempdir");
    let fixture_dir = data_dir.path().join("fixtures");
    std::fs::create_dir_all(&fixture_dir).expect("create fixture dir");
    let bridge_fixture = fixture_dir.join("acp-crp-bridge");
    let amp_fixture = fixture_dir.join("amp-acp.js");
    let pi_fixture = fixture_dir.join("pi-acp.js");
    write_executable(&bridge_fixture, "#!/bin/sh\nexit 0\n");
    write_js_entrypoint(&amp_fixture);
    write_js_entrypoint(&pi_fixture);
    let _matrix_fixture = activate_matrix_fixture(
        data_dir.path(),
        &ProviderMatrix {
            version: fixture_matrix_version(),
            generated_at: None,
            providers: vec![
                bridge_fixture_entry(file_url(&bridge_fixture)),
                archive_js_harness_fixture_entry(
                    "amp",
                    "0.1.2",
                    file_url(&amp_fixture),
                    "dist/bin/amp-acp.js",
                    Some("0.1.0-fixture"),
                ),
                archive_js_harness_fixture_entry(
                    "pi",
                    "0.1.1",
                    file_url(&pi_fixture),
                    "dist/bin/pi-acp.js",
                    Some("0.56.3"),
                ),
            ],
        },
    )
    .await;
    let mut cfg = load_agent_server_config(data_dir.path())
        .await
        .unwrap_or_default();
    seed_managed_node_runtime_metadata(&mut cfg, data_dir.path());
    save_agent_server_config(data_dir.path(), &cfg)
        .await
        .expect("save seeded node runtimes");

    let fixture = providerless_install_fixture(data_dir.path()).await;
    let state = fixture.daemon.clone();
    let app = fixture.router();

    let (install_status, install_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        "/api/providers/install_all?target=container",
        None,
    )
    .await;
    assert_eq!(
        install_status,
        StatusCode::OK,
        "bulk install should start successfully for JS archive harnesses: {install_body:#?}"
    );
    let install_ids = parse_install_ids(&install_body);
    if !install_ids.contains_key("amp") || !install_ids.contains_key("pi") {
        let (amp_status, amp_body): (StatusCode, serde_json::Value) = common::json_request(
            &app,
            axum::http::Method::GET,
            "/api/providers/amp?target=container",
            None,
        )
        .await;
        let (pi_status, pi_body): (StatusCode, serde_json::Value) = common::json_request(
            &app,
            axum::http::Method::GET,
            "/api/providers/pi?target=container",
            None,
        )
        .await;
        panic!(
            "missing JS archive harness install ids from bulk response: {install_body:#?}\namp: ({amp_status}) {amp_body:#?}\npi: ({pi_status}) {pi_body:#?}"
        );
    }
    for provider_id in ["amp", "pi"] {
        let install_id = *install_ids
            .get(provider_id)
            .expect("validated install ids above");
        let install_info =
            wait_for_install_completion_with_timeout(&state, install_id, Duration::from_secs(120))
                .await;
        assert!(
            matches!(install_info.state, InstallStateKind::Succeeded),
            "{provider_id} install should succeed: {install_info:#?}"
        );
    }

    let reloaded_fixture = reopen_providerless_install_fixture(data_dir.path()).await;
    let reloaded_app = reloaded_fixture.router();

    for (provider_id, expected_version) in [("amp", "0.1.2"), ("pi", "0.1.1")] {
        let (provider_status, provider_body): (StatusCode, serde_json::Value) =
            common::json_request(
                &reloaded_app,
                axum::http::Method::GET,
                format!("/api/providers/{provider_id}?target=container"),
                None,
            )
            .await;
        assert_eq!(
            provider_status,
            StatusCode::OK,
            "provider status failed after successful install_all for {provider_id}: {provider_body:#?}"
        );
        assert_eq!(
            provider_body
                .get("installed")
                .and_then(serde_json::Value::as_bool),
            Some(true),
            "{provider_id} should stay installed after install_all: {provider_body:#?}"
        );
        assert_eq!(
            provider_body
                .get("version")
                .and_then(serde_json::Value::as_str),
            Some(expected_version),
            "{provider_id} should report the installed managed version after install_all: {provider_body:#?}"
        );
        assert_eq!(
            provider_body
                .pointer("/details/managed_target")
                .and_then(serde_json::Value::as_str),
            Some("container"),
            "{provider_id} should remain target-scoped to container: {provider_body:#?}"
        );
        assert_eq!(
            provider_body
                .pointer("/details/ready_for_use")
                .and_then(serde_json::Value::as_str),
            Some("true"),
            "{provider_id} should remain ready after install_all: {provider_body:#?}"
        );
        assert!(
            provider_body
                .pointer("/details/managed_dependency_update_available")
                .is_none(),
            "{provider_id} should not claim a dependency update is still needed after a successful install: {provider_body:#?}"
        );
        assert!(
            provider_body
                .pointer("/details/matrix_update_available")
                .is_none(),
            "{provider_id} should not revert to Update after a successful install: {provider_body:#?}"
        );
    }
}

#[tokio::test]
async fn provider_target_scoped_installs_install_all_repairs_invalid_bridge_when_acp_dependents_precede_bridge_in_matrix(
) {
    let _install_lock = provider_install_test_lock().lock().await;
    let _bundle_env = clear_bundle_matrix_env();
    let data_dir = tempfile::tempdir().expect("tempdir");
    let fixture_dir = data_dir.path().join("fixtures");
    std::fs::create_dir_all(&fixture_dir).expect("create fixture dir");
    let bridge_fixture = fixture_dir.join("acp-crp-bridge");
    let kimi_fixture = fixture_dir.join("kimi-acp");
    let qwen_fixture = fixture_dir.join("qwen-acp");
    write_executable(&bridge_fixture, "#!/bin/sh\nsleep 0.3\nexit 0\n");
    write_executable(&kimi_fixture, "#!/bin/sh\nexit 0\n");
    write_executable(&qwen_fixture, "#!/bin/sh\nexit 0\n");
    let download_server = spawn_download_fixture_server(vec![
        (
            "bridge",
            std::fs::read(&bridge_fixture).expect("read bridge fixture"),
            1_600,
        ),
        (
            "kimi",
            std::fs::read(&kimi_fixture).expect("read kimi fixture"),
            0,
        ),
        (
            "qwen",
            std::fs::read(&qwen_fixture).expect("read qwen fixture"),
            0,
        ),
    ])
    .await;
    let _matrix_fixture = activate_matrix_fixture(
        data_dir.path(),
        &ProviderMatrix {
            version: fixture_matrix_version(),
            generated_at: None,
            providers: vec![
                acp_provider_fixture_entry("kimi", fixture_download_url(&download_server, "kimi")),
                acp_provider_fixture_entry("qwen", fixture_download_url(&download_server, "qwen")),
                bridge_fixture_entry(fixture_download_url(&download_server, "bridge")),
            ],
        },
    )
    .await;

    save_invalid_container_bridge_runtime(data_dir.path()).await;

    let fixture = providerless_install_fixture(data_dir.path()).await;
    let state = fixture.daemon.clone();
    let app = fixture.router();

    let (install_status, install_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        "/api/providers/install_all?target=container",
        None,
    )
    .await;
    assert_eq!(
        install_status,
        StatusCode::OK,
        "bulk install should repair an invalid bridge even when ACP providers are listed first: {install_body:#?}"
    );
    let install_ids = parse_install_ids(&install_body);
    assert_eq!(
        install_ids.len(),
        3,
        "install_all should still return the bridge repair plus both ACP harness installs: {install_body:#?}"
    );
    let bridge_install_id = *install_ids
        .get("acp-crp-bridge")
        .expect("missing bridge install id");
    let kimi_install_id = *install_ids.get("kimi").expect("missing kimi install id");
    let qwen_install_id = *install_ids.get("qwen").expect("missing qwen install id");

    let bridge_install_info = wait_for_install_completion(&state, bridge_install_id).await;
    assert!(
        matches!(bridge_install_info.state, InstallStateKind::Succeeded),
        "bridge repair should succeed after being started implicitly by the first ACP install: {bridge_install_info:#?}"
    );
    for (provider_id, install_id) in [("kimi", kimi_install_id), ("qwen", qwen_install_id)] {
        let install_info = wait_for_install_completion(&state, install_id).await;
        assert!(
            matches!(install_info.state, InstallStateKind::Succeeded),
            "{provider_id} should succeed after the repaired bulk install finishes: {install_info:#?}"
        );
    }

    let bridge_install_ids = state
        .provider_target_tracked_install_ids_for_test(
            "acp-crp-bridge",
            Some(InstallTarget::Container),
        )
        .await;
    assert_eq!(
        bridge_install_ids,
        vec![bridge_install_id],
        "deferred ACP installs should reuse one tracked bridge repair install even when the bridge is listed after them"
    );
}

#[tokio::test]
async fn acp_container_install_happy_path_installs_bridge_prerequisite_and_keeps_registry_entries()
{
    let _install_lock = provider_install_test_lock().lock().await;
    let _bundle_env = clear_bundle_matrix_env();
    let data_dir = tempfile::tempdir().expect("tempdir");
    let fixture_dir = data_dir.path().join("fixtures");
    std::fs::create_dir_all(&fixture_dir).expect("create fixture dir");
    let bridge_fixture = fixture_dir.join("acp-crp-bridge");
    let provider_fixture = fixture_dir.join("kimi-acp");
    write_executable(&bridge_fixture, "#!/bin/sh\nexit 0\n");
    write_executable(&provider_fixture, "#!/bin/sh\nsleep 0.5\nexit 0\n");
    let _matrix_fixture = activate_matrix_fixture(
        data_dir.path(),
        &provider_fixture_matrix(file_url(&bridge_fixture), file_url(&provider_fixture)),
    )
    .await;

    let fixture = providerless_install_fixture(data_dir.path()).await;
    let state = fixture.daemon.clone();
    let app = fixture.router();

    let (install_status, install_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        "/api/providers/kimi/install?target=container",
        None,
    )
    .await;
    assert_eq!(
        install_status,
        StatusCode::OK,
        "install should start successfully: {install_body:#?}"
    );
    let install_id = install_body
        .get("install_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|raw| raw.parse::<InstallId>().ok())
        .expect("install id");

    let install_info = wait_for_install_completion(&state, install_id).await;
    assert!(
        matches!(install_info.state, InstallStateKind::Succeeded),
        "kimi install should succeed with bridge prerequisite: {install_info:#?}"
    );

    let bridge_install_id = state
        .provider_target_tracked_install_ids_for_test(
            "acp-crp-bridge",
            Some(InstallTarget::Container),
        )
        .await
        .into_iter()
        .next()
        .expect("bridge prerequisite install entry");
    let bridge_install = state
        .provider_target_install_info_for_test(bridge_install_id)
        .await
        .expect("bridge prerequisite install entry");
    assert!(
        matches!(bridge_install.state, InstallStateKind::Succeeded),
        "bridge prerequisite install should be tracked and succeed: {bridge_install:#?}"
    );
    let cfg = load_agent_server_config(data_dir.path())
        .await
        .expect("load agent server config");
    assert!(
        cfg.managed_provider_targets
            .get("acp-crp-bridge")
            .and_then(|targets| targets.get("container"))
            .is_some(),
        "bridge target-scoped runtime command should remain registered"
    );
    assert!(
        cfg.managed_provider_targets
            .get("kimi")
            .and_then(|targets| targets.get("container"))
            .is_some(),
        "provider target-scoped runtime command should remain registered"
    );
    assert!(
        cfg.managed_install_targets
            .get("acp-crp-bridge")
            .and_then(|targets| targets.get("container"))
            .is_some(),
        "bridge target-scoped install metadata should remain registered"
    );
    assert!(
        cfg.managed_install_targets
            .get("kimi")
            .and_then(|targets| targets.get("container"))
            .is_some(),
        "provider target-scoped install metadata should remain registered"
    );

    let reloaded_fixture = reopen_providerless_install_fixture(data_dir.path()).await;
    let reloaded_app = reloaded_fixture.router();

    let (provider_status, provider_body): (StatusCode, serde_json::Value) = common::json_request(
        &reloaded_app,
        axum::http::Method::GET,
        "/api/providers/kimi?target=container",
        None,
    )
    .await;
    assert_eq!(
        provider_status,
        StatusCode::OK,
        "provider status failed after install: {provider_body:#?}"
    );
    assert_eq!(
        provider_body
            .get("installed")
            .and_then(serde_json::Value::as_bool),
        Some(true),
        "provider should be installed after happy-path bridge prerequisite install: {provider_body:#?}"
    );
    assert_eq!(
        provider_body
            .pointer("/details/ready_for_use")
            .and_then(serde_json::Value::as_str),
        Some("true"),
        "provider should be ready after the happy-path install completes: {provider_body:#?}"
    );
    assert_eq!(
        provider_body
            .pointer("/details/managed_target")
            .and_then(serde_json::Value::as_str),
        Some("container"),
        "provider should keep its container managed-target record: {provider_body:#?}"
    );
    assert!(
        provider_body
            .pointer("/details/managed_checksum_mismatch")
            .is_none(),
        "successful archive installs must not report checksum drift: {provider_body:#?}"
    );
}

#[tokio::test]
async fn tracked_provider_install_surfaces_agent_server_config_errors() {
    let _install_lock = provider_install_test_lock().lock().await;
    let _bundle_env = clear_bundle_matrix_env();
    let data_dir = tempfile::tempdir().expect("tempdir");
    let fixture_dir = data_dir.path().join("fixtures");
    std::fs::create_dir_all(&fixture_dir).expect("create fixture dir");
    let bridge_fixture = fixture_dir.join("acp-crp-bridge");
    let provider_fixture = fixture_dir.join("kimi-acp");
    write_executable(&bridge_fixture, "#!/bin/sh\nexit 0\n");
    write_executable(&provider_fixture, "#!/bin/sh\nexit 0\n");
    let _matrix_fixture = activate_matrix_fixture(
        data_dir.path(),
        &provider_fixture_matrix(file_url(&bridge_fixture), file_url(&provider_fixture)),
    )
    .await;
    write_invalid_agent_server_config(data_dir.path()).await;

    let fixture = providerless_install_fixture(data_dir.path()).await;
    let state = fixture.daemon.clone();
    let (install_id, started_new) = state
        .provider_target_start_tracked_install_for_test(
            "kimi".to_string(),
            Some(InstallTarget::Container),
        )
        .await;
    assert!(started_new, "tracked install should start cleanly");

    let err = state
        .provider_target_install_with_progress_for_test(
            install_id,
            "kimi".to_string(),
            InstallTarget::Container,
        )
        .await
        .expect_err("invalid managed config should fail tracked install");
    let err_text = format!("{err:#}");
    assert!(
        err_text.contains("loading agent server config for provider install contract resolution")
            && err_text.contains("parsing agent server config"),
        "tracked install should surface the managed config parse error chain: {err_text}"
    );

    let install_info = state
        .provider_target_install_info_for_test(install_id)
        .await
        .expect("install info should be recorded");
    assert!(
        matches!(install_info.state, InstallStateKind::Failed),
        "tracked install should be marked failed: {install_info:#?}"
    );
    assert!(
        install_info
            .error
            .as_deref()
            .is_some_and(|value| value.contains("parsing agent server config")),
        "tracked install failure should persist the managed config error: {install_info:#?}"
    );
    assert_ne!(
        install_info.error_code,
        Some(InstallErrorCode::RegistryWriteFailed),
        "tracked install should not misclassify invalid config as a registry write failure: {install_info:#?}"
    );
}

#[tokio::test]
async fn invalid_managed_config_container_routes_do_not_seed_target_adapter_cache() {
    let _install_lock = provider_install_test_lock().lock().await;
    let data_dir = tempfile::tempdir().expect("tempdir");
    configure_container_image_defaults(data_dir.path()).await;
    write_invalid_agent_server_config(data_dir.path()).await;

    let repo = common::init_git_repo(&[("note.txt", "container\n")]).await;
    let fixture = providerless_install_fixture(data_dir.path()).await;
    let state = fixture.daemon.clone();
    let app = fixture.router();
    let workspace = common::create_workspace(&app, repo.path(), "container-ws").await;
    write_workspace_container_execution_without_runtime_probe(&state, workspace.id.0).await;

    let (options_status, options_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        format!("/api/workspaces/{}/providers/qwen/options", workspace.id.0),
        None,
    )
    .await;
    assert_eq!(
        options_status,
        StatusCode::OK,
        "container options route failed: {options_body:#?}"
    );
    assert!(
        options_body["config_error"]
            .as_str()
            .is_some_and(|value| value.contains("parsing agent server config")),
        "container options route should surface managed config parse errors: {options_body:#?}"
    );
    assert_target_adapter_not_cached(
        &state,
        "qwen",
        InstallTarget::Container,
        "container options route",
    )
    .await;

    let (verify_status, verify_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        format!("/api/workspaces/{}/providers/qwen/verify", workspace.id.0),
        None,
    )
    .await;
    assert_eq!(
        verify_status,
        StatusCode::OK,
        "container verify route failed: {verify_body:#?}"
    );
    assert!(
        verify_body["message"]
            .as_str()
            .is_some_and(|value| value.contains("parsing agent server config")),
        "container verify route should surface managed config parse errors: {verify_body:#?}"
    );
    assert_target_adapter_not_cached(
        &state,
        "qwen",
        InstallTarget::Container,
        "container verify route",
    )
    .await;

    let (provider_status, provider_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        "/api/providers/qwen?target=container",
        None,
    )
    .await;
    assert_eq!(
        provider_status,
        StatusCode::OK,
        "container provider status route failed: {provider_body:#?}"
    );
    assert_eq!(
        provider_body
            .pointer("/usability/reason_code")
            .and_then(serde_json::Value::as_str),
        Some("managed_config_error"),
        "container provider status route should surface managed-config errors: {provider_body:#?}"
    );
    assert_target_adapter_not_cached(
        &state,
        "qwen",
        InstallTarget::Container,
        "container provider status route",
    )
    .await;
}

#[tokio::test]
async fn acp_container_install_repairs_invalid_bridge_runtime_and_keeps_registry_entries() {
    let _install_lock = provider_install_test_lock().lock().await;
    let _bundle_env = clear_bundle_matrix_env();
    let data_dir = tempfile::tempdir().expect("tempdir");
    let fixture_dir = data_dir.path().join("fixtures");
    std::fs::create_dir_all(&fixture_dir).expect("create fixture dir");
    let bridge_fixture = fixture_dir.join("acp-crp-bridge");
    let provider_fixture = fixture_dir.join("kimi-acp");
    write_executable(&bridge_fixture, "#!/bin/sh\nexit 0\n");
    write_executable(&provider_fixture, "#!/bin/sh\nsleep 0.5\nexit 0\n");
    let _matrix_fixture = activate_matrix_fixture(
        data_dir.path(),
        &provider_fixture_matrix(file_url(&bridge_fixture), file_url(&provider_fixture)),
    )
    .await;
    save_invalid_container_bridge_runtime(data_dir.path()).await;

    let fixture = providerless_install_fixture(data_dir.path()).await;
    let state = fixture.daemon.clone();
    let app = fixture.router();

    let (install_status, install_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        "/api/providers/kimi/install?target=container",
        None,
    )
    .await;
    assert_eq!(
        install_status,
        StatusCode::OK,
        "single-provider install should repair the invalid managed bridge runtime: {install_body:#?}"
    );
    let install_id = install_body
        .get("install_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|raw| raw.parse::<InstallId>().ok())
        .expect("install id");

    let install_info = wait_for_install_completion(&state, install_id).await;
    assert!(
        matches!(install_info.state, InstallStateKind::Succeeded),
        "kimi install should succeed after repairing the bridge prerequisite: {install_info:#?}"
    );

    let cfg = load_agent_server_config(data_dir.path())
        .await
        .expect("load agent server config");
    let bridge_command = cfg
        .managed_provider_targets
        .get("acp-crp-bridge")
        .and_then(|targets| targets.get("container"))
        .expect("bridge target-scoped runtime command");
    assert_ne!(
        bridge_command.command, "relative-bridge",
        "bridge install should replace the stale invalid managed command"
    );
    assert!(
        Path::new(&bridge_command.command).exists(),
        "bridge install should rewrite the managed command to an on-disk binary: {}",
        bridge_command.command
    );
    assert!(
        cfg.managed_provider_targets
            .get("kimi")
            .and_then(|targets| targets.get("container"))
            .is_some(),
        "provider target-scoped runtime command should remain registered after bridge repair"
    );
}

#[tokio::test]
async fn acp_container_install_parent_polling_stays_bounded_while_bridge_prerequisite_runs() {
    let _install_lock = provider_install_test_lock().lock().await;
    let _bundle_env = clear_bundle_matrix_env();
    let data_dir = tempfile::tempdir().expect("tempdir");
    let fixture_dir = data_dir.path().join("fixtures");
    std::fs::create_dir_all(&fixture_dir).expect("create fixture dir");
    let bridge_fixture = fixture_dir.join("acp-crp-bridge");
    let provider_fixture = fixture_dir.join("kimi-acp");
    write_executable(&bridge_fixture, "#!/bin/sh\nsleep 3.2\nexit 0\n");
    write_executable(&provider_fixture, "#!/bin/sh\nexit 0\n");
    let download_server = spawn_download_fixture_server(vec![
        (
            "bridge",
            std::fs::read(&bridge_fixture).expect("read bridge fixture"),
            3_200,
        ),
        (
            "provider",
            std::fs::read(&provider_fixture).expect("read provider fixture"),
            0,
        ),
    ])
    .await;
    let _matrix_fixture = activate_matrix_fixture(
        data_dir.path(),
        &provider_fixture_matrix(
            fixture_download_url(&download_server, "bridge"),
            fixture_download_url(&download_server, "provider"),
        ),
    )
    .await;

    let fixture = providerless_install_fixture(data_dir.path()).await;
    let state = fixture.daemon.clone();
    let app = fixture.router();

    let (install_status, install_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        "/api/providers/kimi/install?target=container",
        None,
    )
    .await;
    assert_eq!(
        install_status,
        StatusCode::OK,
        "kimi install should start successfully: {install_body:#?}"
    );
    let install_id = install_body
        .get("install_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|raw| raw.parse::<InstallId>().ok())
        .expect("install id");

    let bridge_install_id =
        wait_for_running_install_id(&state, "acp-crp-bridge", Some(InstallTarget::Container)).await;
    let _ = wait_for_running_install_progress(&state, bridge_install_id).await;
    let polled_info =
        wait_for_prerequisite_visibility(&state, &app, install_id, bridge_install_id).await;
    assert!(
        matches!(polled_info.state, InstallStateKind::Running),
        "install should still be running while the bridge prerequisite is active: {polled_info:#?}"
    );
    assert!(
        polled_info
            .last_event
            .as_ref()
            .is_some_and(|event| matches!(event.stage.as_str(), "start" | "prerequisites")),
        "parent poll surface should keep prerequisite progress bounded while the bridge prerequisite runs: {polled_info:#?}"
    );
    assert_eq!(
        compute_polled_install_pct(&polled_info, None),
        Some(2),
        "workbench/settings polling should observe bounded parent progress while the bridge prerequisite runs: {polled_info:#?}"
    );
    assert!(
        polled_info
            .last_event
            .as_ref()
            .is_some_and(|event| event.message.contains("acp-crp-bridge")),
        "parent poll should still expose prerequisite bridge activity: {polled_info:#?}"
    );

    let parent_events = get_install_events_api(&app, install_id).await;
    assert!(
        parent_events.iter().any(|event| {
            event.message.contains("acp-crp-bridge")
                && event.message.contains(&bridge_install_id.to_string())
                && matches!(event.stage.as_str(), "start" | "prerequisites")
        }),
        "parent install events should preserve prerequisite visibility via the real API surface: {parent_events:#?}"
    );

    let install_info =
        wait_for_install_completion_with_timeout(&state, install_id, Duration::from_secs(60)).await;
    assert!(
        matches!(install_info.state, InstallStateKind::Succeeded),
        "kimi install should succeed after the bridge prerequisite finishes: {install_info:#?}"
    );
}

#[tokio::test]
async fn acp_container_install_joins_existing_bridge_install_and_surfaces_short_prerequisites_to_polling(
) {
    let _install_lock = provider_install_test_lock().lock().await;
    let _bundle_env = clear_bundle_matrix_env();
    let data_dir = tempfile::tempdir().expect("tempdir");
    let fixture_dir = data_dir.path().join("fixtures");
    std::fs::create_dir_all(&fixture_dir).expect("create fixture dir");
    let bridge_fixture = fixture_dir.join("acp-crp-bridge");
    let provider_fixture = fixture_dir.join("kimi-acp");
    write_executable(&bridge_fixture, "#!/bin/sh\nsleep 0.2\nexit 0\n");
    write_executable(&provider_fixture, "#!/bin/sh\nexit 0\n");
    let download_server = spawn_download_fixture_server(vec![
        (
            "bridge",
            std::fs::read(&bridge_fixture).expect("read bridge fixture"),
            200,
        ),
        (
            "provider",
            std::fs::read(&provider_fixture).expect("read provider fixture"),
            3_000,
        ),
    ])
    .await;
    let _matrix_fixture = activate_matrix_fixture(
        data_dir.path(),
        &provider_fixture_matrix(
            fixture_download_url(&download_server, "bridge"),
            fixture_download_url(&download_server, "provider"),
        ),
    )
    .await;

    let fixture = providerless_install_fixture(data_dir.path()).await;
    let state = fixture.daemon.clone();
    let app = fixture.router();

    let (bridge_status, bridge_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        "/api/providers/acp-crp-bridge/install?target=container",
        None,
    )
    .await;
    assert_eq!(
        bridge_status,
        StatusCode::OK,
        "bridge install should start successfully: {bridge_body:#?}"
    );
    let bridge_install_id = bridge_body
        .get("install_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|raw| raw.parse::<InstallId>().ok())
        .expect("bridge install id");
    // The throttled fixture server can delay the first observable progress event under full-suite load.
    let _ = wait_for_running_install_progress(&state, bridge_install_id).await;

    let (install_status, install_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        "/api/providers/kimi/install?target=container",
        None,
    )
    .await;
    assert_eq!(
        install_status,
        StatusCode::OK,
        "kimi install should join the running bridge prerequisite: {install_body:#?}"
    );
    let install_id = install_body
        .get("install_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|raw| raw.parse::<InstallId>().ok())
        .expect("install id");

    let _ = wait_for_prerequisite_visibility(&state, &app, install_id, bridge_install_id).await;
    let reader_a = app.clone();
    let reader_b = app.clone();
    let (polled_info_a, polled_info_b) = tokio::join!(
        get_install_info_api(&reader_a, install_id),
        get_install_info_api(&reader_b, install_id)
    );
    for polled_info in [&polled_info_a, &polled_info_b] {
        assert!(
            matches!(polled_info.state, InstallStateKind::Running),
            "install should still be running on the first polling tick: {polled_info:#?}"
        );
        assert!(
            polled_info
                .last_event
                .as_ref()
                .is_some_and(|event| matches!(event.stage.as_str(), "start" | "prerequisites")),
            "short prerequisite installs must still leave a bounded visible parent stage on the first poll: {polled_info:#?}"
        );
        assert_eq!(
            compute_polled_install_pct(polled_info, None),
            Some(2),
            "short bridge prerequisites should remain visible across the shipped polling cadence without overstating progress: {polled_info:#?}"
        );
        assert!(
            polled_info
                .last_event
                .as_ref()
                .is_some_and(|event| {
                    event.message.contains("acp-crp-bridge")
                        && event.message.contains(&bridge_install_id.to_string())
                }),
            "the first poll should still be showing prerequisite-derived progress, not a rewritten parent event: {polled_info:#?}"
        );
    }

    let parent_events = get_install_events_api(&app, install_id).await;
    assert!(
        parent_events.iter().any(|event| {
            event.message.contains("acp-crp-bridge")
                && event.message.contains(&bridge_install_id.to_string())
                && matches!(event.stage.as_str(), "start" | "prerequisites")
        }),
        "parent install events should retain short prerequisite visibility on the real API surface: {parent_events:#?}"
    );

    let parent_owned_poll_info = {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let info = get_install_info_api(&app, install_id).await;
            if matches!(
                info.state,
                InstallStateKind::Succeeded | InstallStateKind::Failed
            ) || info
                .last_event
                .as_ref()
                .is_some_and(|event| !event.message.to_ascii_lowercase().contains("prerequisite"))
            {
                break info;
            }
            assert!(
                    tokio::time::Instant::now() < deadline,
                    "timed out waiting for parent-owned running progress on the poll surface: {info:#?}"
                );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    assert!(
        matches!(
            parent_owned_poll_info.state,
            InstallStateKind::Running | InstallStateKind::Succeeded
        ),
        "the parent poll surface should switch off prerequisite-derived progress once the override window expires: {parent_owned_poll_info:#?}"
    );
    if matches!(parent_owned_poll_info.state, InstallStateKind::Running) {
        assert!(
            parent_owned_poll_info
                .last_event
                .as_ref()
                .is_some_and(|event| !event.message.to_ascii_lowercase().contains("prerequisite")),
            "once the prerequisite override window expires, polling should surface the parent install's own work: {parent_owned_poll_info:#?}"
        );
        assert!(
            parent_owned_poll_info
                .last_event
                .as_ref()
                .is_some_and(|event| event.stage != "prerequisites"),
            "the next poll after the prerequisite window should expose the parent install's own work instead of staying on the synthetic prerequisite stage: {parent_owned_poll_info:#?}"
        );
    }

    let install_info = wait_for_install_completion(&state, install_id).await;
    assert!(
        matches!(install_info.state, InstallStateKind::Succeeded),
        "kimi install should succeed after joining the bridge prerequisite: {install_info:#?}"
    );
    let final_parent_events = get_install_events_api(&app, install_id).await;
    assert!(
        final_parent_events.iter().any(|event| {
            !event.message.starts_with("Prerequisite ") && event.stage == "download"
        }),
        "the parent install event history should still record the parent-owned download stage after the prerequisite handoff: {final_parent_events:#?}"
    );
    assert_eq!(
        final_parent_events
            .iter()
            .filter(|event| event.stage == "start" && !event.message.starts_with("Prerequisite "))
            .count(),
        1,
        "the parent install should keep exactly one parent-owned start event in history: {final_parent_events:#?}"
    );
    assert!(
        final_parent_events.iter().any(|event| {
            event.stage == "start"
                && !event.message.starts_with("Prerequisite ")
                && event.message.contains("Installing managed provider: kimi")
                && event.message.contains("target: container")
        }),
        "the parent install should preserve its richer canonical start message: {final_parent_events:#?}"
    );

    let bridge_info = state
        .provider_target_install_info_for_test(bridge_install_id)
        .await
        .expect("missing bridge install info");
    assert!(
        matches!(bridge_info.state, InstallStateKind::Succeeded),
        "bridge prerequisite install should remain tracked as succeeded: {bridge_info:#?}"
    );

    let bridge_install_ids = state
        .provider_target_tracked_install_ids_for_test(
            "acp-crp-bridge",
            Some(InstallTarget::Container),
        )
        .await;
    assert_eq!(
        bridge_install_ids,
        vec![bridge_install_id],
        "joining ACP installs must reuse the same tracked bridge install id"
    );
}

#[tokio::test]
async fn claude_container_install_starts_host_cli_dependency_and_stays_not_ready_until_it_finishes()
{
    let _install_lock = provider_install_test_lock().lock().await;
    let _bundle_env = clear_bundle_matrix_env();
    let data_dir = tempfile::tempdir().expect("tempdir");
    let fixture_dir = data_dir.path().join("fixtures");
    std::fs::create_dir_all(&fixture_dir).expect("create fixture dir");
    let claude_cli_fixture = fixture_dir.join("claude-cli");
    let claude_crp_fixture = fixture_dir.join("claude-crp");
    write_executable(&claude_cli_fixture, "#!/bin/sh\nsleep 1.6\nexit 0\n");
    write_executable(&claude_crp_fixture, "#!/bin/sh\nexit 0\n");
    let download_server = spawn_download_fixture_server(vec![
        (
            "claude-cli",
            std::fs::read(&claude_cli_fixture).expect("read claude-cli fixture"),
            3_200,
        ),
        (
            "claude-crp",
            std::fs::read(&claude_crp_fixture).expect("read claude-crp fixture"),
            0,
        ),
    ])
    .await;
    let mut claude_crp = archive_fixture_entry(
        "claude-crp",
        ProviderMatrixEntryKind::Harness,
        fixture_download_url(&download_server, "claude-crp"),
    );
    claude_crp.provider_dependencies = vec![MatrixProviderInstallDependency {
        id: "claude-cli".to_string(),
        role: ProviderInstallDependencyRole::Readiness,
        target: ProviderInstallDependencyTarget::Host,
    }];
    let _matrix_fixture = activate_matrix_fixture(
        data_dir.path(),
        &ProviderMatrix {
            version: fixture_matrix_version(),
            generated_at: None,
            providers: vec![
                claude_crp,
                archive_fixture_entry(
                    "claude-cli",
                    ProviderMatrixEntryKind::Dependency,
                    fixture_download_url(&download_server, "claude-cli"),
                ),
            ],
        },
    )
    .await;

    let fixture = providerless_install_fixture(data_dir.path()).await;
    let state = fixture.daemon.clone();
    let app = fixture.router();

    let (install_status, install_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        "/api/providers/claude-crp/install?target=container",
        None,
    )
    .await;
    assert_eq!(
        install_status,
        StatusCode::OK,
        "claude install should start successfully: {install_body:#?}"
    );
    let install_id = install_body
        .get("install_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|raw| raw.parse::<InstallId>().ok())
        .expect("install id");

    let claude_cli_install_id =
        wait_for_tracked_install_id(&state, "claude-cli", Some(InstallTarget::Host)).await;

    let visibility_deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    let (mut parent_poll, parent_status_body) = loop {
        let dependency_info = state
            .provider_target_install_info_for_test(claude_cli_install_id)
            .await
            .expect("missing claude-cli dependency install info");
        let parent_poll = get_install_info_api(&app, install_id).await;
        let (provider_status, provider_body): (StatusCode, serde_json::Value) =
            common::json_request(
                &app,
                axum::http::Method::GET,
                "/api/providers/claude-crp?target=container",
                None,
            )
            .await;
        assert_eq!(
            provider_status,
            StatusCode::OK,
            "provider status failed while claude-cli was still installing: {provider_body:#?}"
        );
        if matches!(dependency_info.state, InstallStateKind::Running)
            && matches!(parent_poll.state, InstallStateKind::Running)
            && parent_poll
                .last_event
                .as_ref()
                .is_some_and(|event| event.message.contains("claude-cli"))
            && provider_body
                .pointer("/details/pending_dependency_ids")
                .and_then(serde_json::Value::as_str)
                == Some("claude-cli")
        {
            break (parent_poll, provider_body);
        }
        assert!(
            tokio::time::Instant::now() < visibility_deadline,
            "timed out waiting for claude readiness gating to surface: {parent_poll:#?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        parent_poll.progress_pct.unwrap_or_default() < 100,
        "parent poll should stay incomplete while claude-cli is still installing: {parent_poll:#?}"
    );
    if parent_poll.progress_pct != Some(99) {
        let progress_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let dependency_info = state
                .provider_target_install_info_for_test(claude_cli_install_id)
                .await
                .expect("missing claude-cli dependency install info");
            parent_poll = get_install_info_api(&app, install_id).await;
            if parent_poll.progress_pct == Some(99) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < progress_deadline
                    && matches!(dependency_info.state, InstallStateKind::Running),
                "timed out waiting for claude readiness progress pin: dependency={dependency_info:#?} parent={parent_poll:#?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    assert_eq!(
        parent_poll.progress_pct,
        Some(99),
        "readiness dependency waiting should pin parent polling progress at 99: {parent_poll:#?}"
    );
    assert!(
        parent_poll
            .last_event
            .as_ref()
            .is_some_and(|event| event.message.contains("claude-cli")),
        "parent poll should expose dependency activity while claude-cli is still installing: {parent_poll:#?}"
    );
    assert_eq!(
        parent_status_body
            .pointer("/details/ready_for_use")
            .and_then(serde_json::Value::as_str),
        Some("false"),
        "claude-crp should stay not-ready until claude-cli finishes: {parent_status_body:#?}"
    );
    assert_eq!(
        parent_status_body
            .pointer("/details/required_dependency_ids")
            .and_then(serde_json::Value::as_str),
        Some("claude-cli"),
        "status should expose the declared Claude dependency set: {parent_status_body:#?}"
    );
    assert_eq!(
        parent_status_body
            .pointer("/details/pending_dependency_ids")
            .and_then(serde_json::Value::as_str),
        Some("claude-cli"),
        "status should keep claude-cli pending until the dependency install completes: {parent_status_body:#?}"
    );
    assert_eq!(
        parent_status_body
            .pointer("/details/install_target")
            .and_then(serde_json::Value::as_str),
        Some("container"),
        "status should remain target-aware while waiting for the host dependency: {parent_status_body:#?}"
    );

    let dependency_info = wait_for_install_completion_with_timeout(
        &state,
        claude_cli_install_id,
        Duration::from_secs(60),
    )
    .await;
    assert!(
        matches!(dependency_info.state, InstallStateKind::Succeeded),
        "claude-cli dependency install should succeed: {dependency_info:#?}"
    );
    let parent_info =
        wait_for_install_completion_with_timeout(&state, install_id, Duration::from_secs(30)).await;
    assert!(
        matches!(parent_info.state, InstallStateKind::Succeeded),
        "claude-crp install should complete after its host dependency finishes: {parent_info:#?}"
    );

    let cfg = load_agent_server_config(data_dir.path())
        .await
        .expect("load agent server config");
    assert!(
        cfg.managed_provider_targets
            .get("claude-crp")
            .and_then(|targets| targets.get("container"))
            .is_some(),
        "claude-crp container runtime should be registered"
    );
    assert!(
        cfg.managed_install_targets
            .get("claude-crp")
            .and_then(|targets| targets.get("container"))
            .is_some(),
        "claude-crp container install metadata should be registered"
    );
    assert!(
        cfg.managed_provider_targets
            .get("claude-cli")
            .and_then(|targets| targets.get("host"))
            .is_some(),
        "claude-cli host runtime should be registered"
    );
    assert!(
        cfg.managed_install_targets
            .get("claude-cli")
            .and_then(|targets| targets.get("host"))
            .is_some(),
        "claude-cli host install metadata should be registered"
    );
    let claude_crp_runtime = cfg
        .managed_provider_targets
        .get("claude-crp")
        .and_then(|targets| targets.get("container"))
        .expect("claude-crp container runtime should exist");
    assert_eq!(
        claude_crp_runtime.dependencies,
        vec!["claude-cli".to_string()],
        "claude-crp runtime should persist the managed dependency edge"
    );

    let reloaded_fixture = reopen_providerless_install_fixture(data_dir.path()).await;
    let reloaded_app = reloaded_fixture.router();
    let (provider_status, provider_body): (StatusCode, serde_json::Value) = common::json_request(
        &reloaded_app,
        axum::http::Method::GET,
        "/api/providers/claude-crp?target=container",
        None,
    )
    .await;
    assert_eq!(
        provider_status,
        StatusCode::OK,
        "provider status failed after claude install completion: {provider_body:#?}"
    );
    assert_eq!(
        provider_body
            .get("installed")
            .and_then(serde_json::Value::as_bool),
        Some(true),
        "claude-crp should remain installed after its dependency completes: {provider_body:#?}"
    );
    assert_eq!(
        provider_body
            .pointer("/details/ready_for_use")
            .and_then(serde_json::Value::as_str),
        Some("true"),
        "claude-crp should become ready once claude-cli finishes: {provider_body:#?}"
    );
    assert!(
        provider_body
            .pointer("/details/managed_checksum_mismatch")
            .is_none(),
        "successful archive installs must not report checksum drift after reload: {provider_body:#?}"
    );
    assert!(
        provider_body.pointer("/details/pending_dependency_ids").is_none(),
        "pending dependency ids should clear once the host dependency is installed: {provider_body:#?}"
    );
}

#[tokio::test]
#[ignore]
async fn provider_target_scoped_installs_work_for_host_and_container_workspaces() {
    let _install_lock = provider_install_test_lock().lock().await;
    let _bundle_env = clear_bundle_matrix_env();
    if std::env::var("CTX_E2E_SANDBOX").ok().as_deref() != Some("1") {
        eprintln!("skipping: CTX_E2E_SANDBOX not set");
        return;
    }
    if sandbox_cli_binary_for_tests().is_none() {
        eprintln!("skipping: sandbox CLI not available");
        return;
    }
    let sandbox_cli = sandbox_cli_binary_for_tests().expect("sandbox CLI not available");
    let _sandbox_cli_path = EnvVarGuard::set(
        "CTX_HARNESS_SANDBOX_CLI_PATH",
        &sandbox_cli.to_string_lossy(),
    );
    if !sandbox_cli_ready(&sandbox_cli).await {
        eprintln!("skipping: sandbox CLI connection is not ready");
        return;
    }
    if !sandbox_cli_has_image(&sandbox_cli, "python:3.11").await {
        eprintln!("skipping: python:3.11 image is not present locally in the sandbox runtime");
        return;
    }

    let data_dir = tempfile::tempdir().expect("tempdir");
    let runtime = seed_target_scoped_codex_runtime(data_dir.path()).await;
    configure_container_image_defaults(data_dir.path()).await;
    let fixture = build_state_with_host_codex(data_dir.path(), &runtime.host_command).await;
    let state = fixture.daemon.clone();
    let app = fixture.router();

    let host_repo = common::init_git_repo(&[("note.txt", "host\n")]).await;
    let container_repo = common::init_git_repo(&[("note.txt", "container\n")]).await;

    let host_ws = common::create_workspace(&app, host_repo.path(), "host-ws").await;
    let container_ws = common::create_workspace(&app, container_repo.path(), "container-ws").await;
    set_workspace_container_execution(&app, container_ws.id.0, "sandbox").await;

    let (host_options_status, host_options): (StatusCode, serde_json::Value) =
        common::json_request(
            &app,
            axum::http::Method::GET,
            format!("/api/workspaces/{}/providers/codex/options", host_ws.id.0),
            None,
        )
        .await;
    assert_eq!(
        host_options_status,
        StatusCode::OK,
        "host options failed: {host_options:#?}"
    );
    assert_eq!(
        host_options
            .pointer("/models/current_model_id")
            .and_then(serde_json::Value::as_str),
        Some("host-model")
    );

    let (container_options_status, container_options): (StatusCode, serde_json::Value) =
        common::json_request(
            &app,
            axum::http::Method::GET,
            format!(
                "/api/workspaces/{}/providers/codex/options",
                container_ws.id.0
            ),
            None,
        )
        .await;
    assert_eq!(
        container_options_status,
        StatusCode::OK,
        "container options failed: {container_options:#?}"
    );
    assert_eq!(
        container_options
            .pointer("/models/current_model_id")
            .and_then(serde_json::Value::as_str),
        Some("container-model"),
        "unexpected container options body: {container_options:#?}"
    );

    let (_host_task, host_session) =
        common::create_task_with_session(&app, host_ws.id.0, "host-task", "codex", "host-model")
            .await;
    let (_container_task, container_session) = common::create_task_with_session(
        &app,
        container_ws.id.0,
        "container-task",
        "codex",
        "container-model",
    )
    .await;

    post_message(&app, host_session.id.0, "reply exactly once").await;
    post_message(&app, container_session.id.0, "reply exactly once").await;
    wait_for_done_with_assistant_message(&state, host_session.id, "host-runtime").await;
    wait_for_done_with_assistant_message(&state, container_session.id, "container-runtime").await;
}
