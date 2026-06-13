use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ctx_provider_install::install_state::InstallTarget;
use ctx_provider_matrix::{
    DependencyInstall, ProviderArchiveKind, ProviderArchiveTarget, ProviderCommand,
    ProviderDependency, ProviderInstall, ProviderMatrixEntry, ProviderMatrixEntryKind,
    ProviderRelease, ProviderReleaseStatus,
};
use ctx_providers::adapters::{ProviderHealth, ProviderStatus, ProviderUsability};
use sha2::{Digest, Sha256};

use crate::{
    provider_status_matrix::{
        apply_matrix_to_status, managed_dependency_update_available, probe_command_version,
        probe_node_package_version,
    },
    AgentServerCommand, AgentServerConfigFile, ManagedInstallMetadata,
};

const CURRENT_CTX_VERSION: Option<&str> = Some("0.59.0-canary.deadbeefcafe");

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

struct ScopedEnvVar {
    key: &'static str,
    old: Option<String>,
}

impl ScopedEnvVar {
    fn set(key: &'static str, value: &str) -> Self {
        let old = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, old }
    }
}

impl Drop for ScopedEnvVar {
    fn drop(&mut self) {
        match &self.old {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

fn release(
    version: &str,
    status: ProviderReleaseStatus,
    context_min: Option<&str>,
) -> ProviderRelease {
    ProviderRelease {
        version: version.to_string(),
        status,
        upstream_version: Some(version.to_string()),
        provenance: None,
        context_min: context_min.map(ToOwned::to_owned),
        context_max: None,
        notes: None,
    }
}

fn codex_archive_entry(
    version: &str,
    sha256: &str,
    releases: Vec<ProviderRelease>,
) -> ProviderMatrixEntry {
    ProviderMatrixEntry {
        id: "codex".to_string(),
        kind: ProviderMatrixEntryKind::Harness,
        display_name: Some("Codex".to_string()),
        tier: Some("tier1".to_string()),
        command: None,
        managed_install: Some(ProviderInstall::Archive {
            version: version.to_string(),
            args: Vec::new(),
            targets: HashMap::from([(
                "linux-x86_64".to_string(),
                ProviderArchiveTarget {
                    url: "https://example.invalid/codex.tar.gz".to_string(),
                    sha256: Some(sha256.to_string()),
                    size_bytes: None,
                    archive: ProviderArchiveKind::TarGz,
                    bin_path: "codex-crp".to_string(),
                },
            )]),
        }),
        provider_dependencies: Vec::new(),
        dependencies: Vec::new(),
        version_probe: None,
        releases,
    }
}

#[cfg(unix)]
#[tokio::test]
async fn provider_version_probe_scrubs_daemon_auth_env() {
    let _env_guard = crate::test_support::process_env_test_lock().lock().await;
    let script = r#"
for key in CTX_AUTH_TOKEN CTX_MCP_TOKEN CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN; do
  eval "value=\${$key:-}"
  if [ -n "$value" ]; then
    echo "unexpected $key" >&2
    exit 91
  fi
done
echo "provider 1.2.3"
"#;

    let _guards: Vec<_> = ctx_core::env::DAEMON_AUTH_ENV_VARS
        .iter()
        .map(|key| ScopedEnvVar::set(key, "daemon-secret"))
        .collect();
    let version = probe_command_version("/bin/sh", &["-c".to_string(), script.to_string()]).await;

    assert_eq!(version.as_deref(), Some("1.2.3"));
}

fn create_gemini_probe_layout(root: &Path) -> (PathBuf, PathBuf) {
    let node_bin = root
        .join("bundle")
        .join("runtimes")
        .join("node")
        .join("bin")
        .join("node");
    let cli_entry = root
        .join("bundle")
        .join("providers")
        .join("gemini")
        .join("node_modules")
        .join("@google")
        .join("gemini-cli")
        .join("bundle")
        .join("gemini.js");
    let cli_pkg = cli_entry
        .parent()
        .expect("bundle dir")
        .parent()
        .expect("cli root")
        .join("package.json");

    std::fs::create_dir_all(node_bin.parent().expect("node parent")).expect("mkdir node");
    std::fs::create_dir_all(cli_entry.parent().expect("cli parent")).expect("mkdir cli");
    std::fs::write(&node_bin, b"node").expect("write node");
    std::fs::write(&cli_entry, b"cli").expect("write cli");
    std::fs::write(
        &cli_pkg,
        r#"{"name":"@google/gemini-cli","version":"0.38.2"}"#,
    )
    .expect("write cli package");

    (node_bin, cli_entry)
}

#[test]
fn probe_node_package_version_uses_explicit_gemini_entrypoint() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (node_bin, cli_entry) = create_gemini_probe_layout(temp.path());
    let command = ProviderCommand {
        command: node_bin.to_string_lossy().to_string(),
        args: vec![cli_entry.to_string_lossy().to_string()],
    };

    let version = probe_node_package_version(&command, "@google/gemini-cli", temp.path());

    assert_eq!(version.as_deref(), Some("0.38.2"));
}

#[test]
fn probe_node_package_version_rejects_path_style_gemini_command() {
    let temp = tempfile::tempdir().expect("tempdir");
    let command = ProviderCommand {
        command: "gemini".to_string(),
        args: vec!["--experimental-acp".to_string()],
    };

    let version = probe_node_package_version(&command, "@google/gemini-cli", temp.path());

    assert!(version.is_none());
}

#[test]
fn probe_node_package_version_rejects_relative_gemini_entrypoint() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (node_bin, _) = create_gemini_probe_layout(temp.path());
    let command = ProviderCommand {
        command: node_bin.to_string_lossy().to_string(),
        args: vec!["node_modules/@google/gemini-cli/bundle/gemini.js".to_string()],
    };

    let version = probe_node_package_version(&command, "@google/gemini-cli", temp.path());

    assert!(version.is_none());
}

fn codex_npm_entry(releases: Vec<ProviderRelease>) -> ProviderMatrixEntry {
    let managed_version = releases
        .last()
        .map(|release| release.version.clone())
        .unwrap_or_else(|| "1.0.0".to_string());
    ProviderMatrixEntry {
        id: "codex".to_string(),
        kind: ProviderMatrixEntryKind::Harness,
        display_name: Some("Codex".to_string()),
        tier: Some("tier1".to_string()),
        command: None,
        managed_install: Some(ProviderInstall::Npm {
            package: "@openai/codex".to_string(),
            version: managed_version,
            entrypoint: "node_modules/@openai/codex/bin.js".to_string(),
            args: Vec::new(),
            targets: std::collections::HashMap::new(),
        }),
        provider_dependencies: Vec::new(),
        dependencies: Vec::new(),
        version_probe: None,
        releases,
    }
}

fn managed_archive_cfg(
    command_path: &Path,
    version: &str,
    installed_sha256: &str,
) -> AgentServerConfigFile {
    let target = InstallTarget::LinuxX8664;
    let target_key = target.as_str().to_string();
    let meta = ManagedInstallMetadata {
        package: Some("https://example.invalid/codex.tar.gz".to_string()),
        version: Some(version.to_string()),
        artifact_fingerprint: Some(installed_sha256.to_string()),
        archive_sha256: Some(installed_sha256.to_string()),
        target: Some(target),
        install_dir_rel: Some(format!("providers/agent-servers/codex/{version}")),
        bin_dir_rel: None,
        last_success_at: None,
        last_error: None,
    };
    let mut cfg = AgentServerConfigFile::default();
    cfg.managed_install_targets.insert(
        "codex".to_string(),
        HashMap::from([(target_key.clone(), meta.clone())]),
    );
    cfg.managed_provider_targets.insert(
        "codex".to_string(),
        HashMap::from([(
            target_key,
            AgentServerCommand {
                command: command_path.to_string_lossy().to_string(),
                args: Vec::new(),
                dependencies: Vec::new(),
                managed: Some(meta),
            },
        )]),
    );
    cfg
}

fn managed_hybrid_npm_container_cfg(
    version: &str,
    installed_sha256: &str,
) -> AgentServerConfigFile {
    let target = InstallTarget::Container;
    let target_key = target.as_str().to_string();
    let meta = ManagedInstallMetadata {
        package: Some("@openai/codex".to_string()),
        version: Some(version.to_string()),
        artifact_fingerprint: Some(installed_sha256.to_string()),
        archive_sha256: Some(installed_sha256.to_string()),
        target: Some(target),
        install_dir_rel: Some(format!("providers/agent-servers/codex/{version}")),
        bin_dir_rel: Some(format!("providers/agent-servers/codex/{version}/bin")),
        last_success_at: None,
        last_error: None,
    };
    let mut cfg = AgentServerConfigFile::default();
    cfg.managed_install_targets.insert(
        "codex".to_string(),
        HashMap::from([(target_key.clone(), meta.clone())]),
    );
    cfg.managed_provider_targets.insert(
        "codex".to_string(),
        HashMap::from([(
            target_key,
            AgentServerCommand {
                command: "/tmp/codex-container".to_string(),
                args: Vec::new(),
                dependencies: Vec::new(),
                managed: Some(meta),
            },
        )]),
    );
    cfg
}

fn managed_npm_cfg(version: &str) -> AgentServerConfigFile {
    let target = InstallTarget::Host;
    let target_key = target.as_str().to_string();
    let meta = ManagedInstallMetadata {
        package: Some("@openai/codex".to_string()),
        version: Some(version.to_string()),
        artifact_fingerprint: Some(format!("npm:@openai/codex@{version}")),
        archive_sha256: None,
        target: Some(target),
        install_dir_rel: Some(format!("providers/agent-servers/codex/{version}")),
        bin_dir_rel: Some(format!("providers/agent-servers/codex/{version}/bin")),
        last_success_at: None,
        last_error: None,
    };
    let mut cfg = AgentServerConfigFile::default();
    cfg.managed_install_targets.insert(
        "codex".to_string(),
        HashMap::from([(target_key.clone(), meta.clone())]),
    );
    cfg.managed_provider_targets.insert(
        "codex".to_string(),
        HashMap::from([(
            target_key,
            AgentServerCommand {
                command: "/tmp/codex".to_string(),
                args: Vec::new(),
                dependencies: Vec::new(),
                managed: Some(meta),
            },
        )]),
    );
    cfg
}

fn installed_status(provider_id: &str, install_target: InstallTarget) -> ProviderStatus {
    ProviderStatus {
        provider_id: provider_id.to_string(),
        installed: true,
        detected_path: Some(format!("/tmp/{provider_id}")),
        version: None,
        capabilities: None,
        health: ProviderHealth::Ok,
        diagnostics: Vec::new(),
        details: HashMap::from([(
            "install_target".to_string(),
            install_target.as_str().to_string(),
        )]),
        usability: ProviderUsability::default(),
    }
}

#[tokio::test]
async fn provider_status_matrix_marks_supported_stale_runtime_updateable_without_blocking() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = temp.path().join("codex");
    std::fs::write(&runtime, b"old-runtime").expect("write runtime");

    let old_sha = sha256_hex(b"old-archive");
    let new_sha = sha256_hex(b"new-archive");
    let entry = codex_archive_entry(
        "1.0.1",
        &new_sha,
        vec![
            release("1.0.0", ProviderReleaseStatus::Supported, None),
            release("1.0.1", ProviderReleaseStatus::Supported, Some("0.59.0")),
        ],
    );
    let cfg = managed_archive_cfg(&runtime, "1.0.0", &old_sha);
    let mut status = installed_status("codex", InstallTarget::LinuxX8664);

    apply_matrix_to_status(temp.path(), &cfg, &entry, &mut status, CURRENT_CTX_VERSION).await;

    assert_eq!(status.version.as_deref(), Some("1.0.0"));
    assert!(matches!(status.health, ProviderHealth::Ok));
    assert_eq!(
        status
            .details
            .get("matrix_recommended_version")
            .map(String::as_str),
        Some("1.0.1")
    );
    assert_eq!(
        status
            .details
            .get("matrix_update_available")
            .map(String::as_str),
        Some("true")
    );
}

#[tokio::test]
async fn provider_status_matrix_marks_missing_runtime_dependency_updateable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = temp.path().join("codex");
    std::fs::write(&runtime, b"matching-runtime").expect("write runtime");

    let sha = sha256_hex(b"matching-archive");
    let entry = codex_archive_entry(
        "1.0.1",
        &sha,
        vec![release("1.0.1", ProviderReleaseStatus::Supported, None)],
    );
    let mut cfg = managed_archive_cfg(&runtime, "1.0.1", &sha);
    cfg.managed_provider_targets
        .get_mut("codex")
        .and_then(|targets| targets.get_mut(InstallTarget::LinuxX8664.as_str()))
        .expect("codex linux target")
        .dependencies = vec!["runtime-node-host".to_string()];
    let mut status = installed_status("codex", InstallTarget::LinuxX8664);

    apply_matrix_to_status(temp.path(), &cfg, &entry, &mut status, CURRENT_CTX_VERSION).await;

    assert!(matches!(status.health, ProviderHealth::Ok));
    assert_eq!(
        status
            .details
            .get("managed_dependency_update_available")
            .map(String::as_str),
        Some("true")
    );
    assert_eq!(
        status
            .details
            .get("matrix_update_available")
            .map(String::as_str),
        Some("true")
    );
}

#[tokio::test]
async fn provider_status_matrix_marks_stale_implicit_node_runtime_updateable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = temp.path().join("codex");
    std::fs::write(&runtime, b"matching-runtime").expect("write runtime");

    let sha = sha256_hex(b"matching-archive");
    let entry = codex_archive_entry(
        "1.0.1",
        &sha,
        vec![release("1.0.1", ProviderReleaseStatus::Supported, None)],
    );
    let mut cfg = managed_archive_cfg(&runtime, "1.0.1", &sha);
    let dependency_id = "runtime-node-linux-x86_64";
    cfg.managed_provider_targets
        .get_mut("codex")
        .and_then(|targets| targets.get_mut(InstallTarget::LinuxX8664.as_str()))
        .expect("codex linux target")
        .dependencies = vec![dependency_id.to_string()];
    cfg.managed_install_targets.insert(
        dependency_id.to_string(),
        HashMap::from([(
            InstallTarget::LinuxX8664.as_str().to_string(),
            ManagedInstallMetadata {
                package: Some("node-runtime".to_string()),
                version: Some(crate::NODE_VERSION.to_string()),
                artifact_fingerprint: Some(format!(
                    "runtime:node:{}:sha256:{}",
                    crate::NODE_VERSION,
                    "0".repeat(64)
                )),
                archive_sha256: Some("0".repeat(64)),
                target: Some(InstallTarget::LinuxX8664),
                install_dir_rel: Some("runtimes/node/stale".to_string()),
                bin_dir_rel: Some("runtimes/node/stale/bin".to_string()),
                last_success_at: None,
                last_error: None,
            },
        )]),
    );
    let mut status = installed_status("codex", InstallTarget::LinuxX8664);

    apply_matrix_to_status(temp.path(), &cfg, &entry, &mut status, CURRENT_CTX_VERSION).await;

    assert!(matches!(status.health, ProviderHealth::Ok));
    assert_eq!(
        status
            .details
            .get("managed_dependency_update_available")
            .map(String::as_str),
        Some("true")
    );
    assert_eq!(
        status
            .details
            .get("matrix_update_available")
            .map(String::as_str),
        Some("true")
    );
}

#[tokio::test]
async fn provider_status_matrix_uses_dependency_id_target_for_implicit_node_runtime() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = temp.path().join("codex");
    std::fs::write(&runtime, b"matching-runtime").expect("write runtime");

    let sha = sha256_hex(b"matching-archive");
    let entry = codex_archive_entry(
        "1.0.1",
        &sha,
        vec![release("1.0.1", ProviderReleaseStatus::Supported, None)],
    );
    let mut cfg = managed_archive_cfg(&runtime, "1.0.1", &sha);
    let dependency_id = "runtime-node-linux-x86_64";
    let linux_runtime_sha = "44836872d9aec49f1e6b52a9a922872db9a2b02d235a616a5681b6a85fec8d89";
    cfg.managed_provider_targets
        .get_mut("codex")
        .and_then(|targets| targets.get_mut(InstallTarget::LinuxX8664.as_str()))
        .expect("codex linux target")
        .dependencies = vec![dependency_id.to_string()];
    cfg.managed_install_targets.insert(
        dependency_id.to_string(),
        HashMap::from([(
            InstallTarget::LinuxX8664.as_str().to_string(),
            ManagedInstallMetadata {
                package: Some("node-runtime".to_string()),
                version: Some(crate::NODE_VERSION.to_string()),
                artifact_fingerprint: Some(format!(
                    "runtime:node:{}:sha256:{linux_runtime_sha}",
                    crate::NODE_VERSION
                )),
                archive_sha256: Some(linux_runtime_sha.to_string()),
                target: None,
                install_dir_rel: Some("runtimes/node/linux".to_string()),
                bin_dir_rel: Some("runtimes/node/linux/bin".to_string()),
                last_success_at: None,
                last_error: None,
            },
        )]),
    );
    let mut status = installed_status("codex", InstallTarget::LinuxX8664);

    apply_matrix_to_status(temp.path(), &cfg, &entry, &mut status, CURRENT_CTX_VERSION).await;

    assert!(matches!(status.health, ProviderHealth::Ok));
    assert!(
        !status
            .details
            .contains_key("managed_dependency_update_available"),
        "matching linux runtime metadata with missing target must not be compared as host"
    );
    assert!(
        !status.details.contains_key("matrix_update_available"),
        "matching implicit runtime dependency should not mark matrix update available"
    );
}

#[tokio::test]
async fn provider_status_matrix_marks_hybrid_container_archive_updates_available() {
    let old_sha = sha256_hex(b"old-container-archive");
    let new_sha = sha256_hex(b"new-container-archive");
    let mut entry = codex_npm_entry(vec![
        release("1.0.0", ProviderReleaseStatus::Supported, None),
        release("1.0.1", ProviderReleaseStatus::Supported, None),
    ]);
    if let Some(ProviderInstall::Npm { targets, .. }) = entry.managed_install.as_mut() {
        targets.insert(
            "linux-x86_64".to_string(),
            ProviderArchiveTarget {
                url: "https://example.invalid/codex-container.tar.gz".to_string(),
                sha256: Some(new_sha.clone()),
                size_bytes: None,
                archive: ProviderArchiveKind::TarGz,
                bin_path: "codex-crp".to_string(),
            },
        );
    }
    let cfg = managed_hybrid_npm_container_cfg("1.0.0", &old_sha);
    let mut status = installed_status("codex", InstallTarget::Container);

    apply_matrix_to_status(
        Path::new("/tmp"),
        &cfg,
        &entry,
        &mut status,
        CURRENT_CTX_VERSION,
    )
    .await;

    assert_eq!(status.version.as_deref(), Some("1.0.0"));
    assert!(matches!(status.health, ProviderHealth::Ok));
    assert_eq!(
        status
            .details
            .get("matrix_recommended_version")
            .map(String::as_str),
        Some("1.0.1")
    );
    assert_eq!(
        status
            .details
            .get("matrix_update_available")
            .map(String::as_str),
        Some("true")
    );
}

#[tokio::test]
async fn provider_status_matrix_marks_latest_ctx_incompatible_release_as_requires_ctx_update_only()
{
    let entry = codex_npm_entry(vec![
        release("1.2.2", ProviderReleaseStatus::Supported, None),
        release("1.2.3", ProviderReleaseStatus::Supported, Some("0.99.0")),
    ]);
    let cfg = managed_npm_cfg("1.2.2");
    let mut status = installed_status("codex", InstallTarget::Host);

    apply_matrix_to_status(
        Path::new("/tmp"),
        &cfg,
        &entry,
        &mut status,
        CURRENT_CTX_VERSION,
    )
    .await;

    assert_eq!(status.version.as_deref(), Some("1.2.2"));
    assert!(matches!(status.health, ProviderHealth::Ok));
    assert_eq!(
        status
            .details
            .get("matrix_recommended_version")
            .map(String::as_str),
        Some("1.2.2")
    );
    assert_eq!(
        status
            .details
            .get("matrix_latest_version")
            .map(String::as_str),
        Some("1.2.3")
    );
    assert_eq!(
        status
            .details
            .get("matrix_update_requires_context")
            .map(String::as_str),
        Some("true")
    );
    assert!(!status.details.contains_key("matrix_update_available"));
}

#[tokio::test]
async fn provider_status_matrix_marks_blocked_installed_release_unsupported() {
    let entry = codex_npm_entry(vec![
        release("1.2.2", ProviderReleaseStatus::Blocked, None),
        release("1.2.3", ProviderReleaseStatus::Supported, None),
    ]);
    let cfg = managed_npm_cfg("1.2.2");
    let mut status = installed_status("codex", InstallTarget::Host);

    apply_matrix_to_status(
        Path::new("/tmp"),
        &cfg,
        &entry,
        &mut status,
        CURRENT_CTX_VERSION,
    )
    .await;

    assert_eq!(status.version.as_deref(), Some("1.2.2"));
    assert!(matches!(status.health, ProviderHealth::UnsupportedVersion));
    assert_eq!(
        status
            .details
            .get("matrix_update_available")
            .map(String::as_str),
        Some("true")
    );
    assert!(status
        .diagnostics
        .iter()
        .any(|message| message.contains("blocked by the support matrix")));
}

#[test]
fn managed_dependency_update_available_when_runtime_dependency_missing() {
    let mut cfg = AgentServerConfigFile::default();
    cfg.providers.insert(
        "codex".to_string(),
        AgentServerCommand {
            command: "/tmp/codex".to_string(),
            args: Vec::new(),
            dependencies: vec!["runtime-node-host".to_string()],
            managed: None,
        },
    );
    let status = installed_status("codex", InstallTarget::Host);

    assert!(managed_dependency_update_available(
        &cfg,
        &codex_archive_entry("1.0.0", &sha256_hex(b"unused"), vec![]),
        &status,
    ));
}

#[test]
fn managed_dependency_update_available_when_runtime_dependency_version_mismatched() {
    let mut cfg = AgentServerConfigFile::default();
    cfg.providers.insert(
        "codex".to_string(),
        AgentServerCommand {
            command: "/tmp/codex".to_string(),
            args: Vec::new(),
            dependencies: vec!["runtime-node-host".to_string()],
            managed: None,
        },
    );
    cfg.managed_installs.insert(
        "runtime-node-host".to_string(),
        ManagedInstallMetadata {
            package: Some("node-runtime".to_string()),
            version: Some("0.0.1".to_string()),
            artifact_fingerprint: None,
            archive_sha256: None,
            target: None,
            install_dir_rel: None,
            bin_dir_rel: None,
            last_success_at: None,
            last_error: None,
        },
    );
    let status = installed_status("codex", InstallTarget::Host);

    assert!(managed_dependency_update_available(
        &cfg,
        &codex_archive_entry("1.0.0", &sha256_hex(b"unused"), vec![]),
        &status,
    ));
}

#[test]
fn managed_dependency_update_unavailable_when_runtime_dependency_matches_expected() {
    let mut cfg = AgentServerConfigFile::default();
    cfg.providers.insert(
        "codex".to_string(),
        AgentServerCommand {
            command: "/tmp/codex".to_string(),
            args: Vec::new(),
            dependencies: vec!["runtime-node-host".to_string()],
            managed: None,
        },
    );
    let expected = crate::expected_managed_dependency_version("runtime-node-host")
        .expect("runtime node version");
    let expected_fingerprint = crate::expected_managed_dependency_artifact_fingerprint_for_id(
        "runtime-node-host",
        None,
        InstallTarget::Host,
    )
    .expect("runtime node fingerprint");
    cfg.managed_installs.insert(
        "runtime-node-host".to_string(),
        ManagedInstallMetadata {
            package: Some("node-runtime".to_string()),
            version: Some(expected.to_string()),
            artifact_fingerprint: Some(expected_fingerprint),
            archive_sha256: None,
            target: None,
            install_dir_rel: None,
            bin_dir_rel: None,
            last_success_at: None,
            last_error: None,
        },
    );
    let status = installed_status("codex", InstallTarget::Host);

    assert!(!managed_dependency_update_available(
        &cfg,
        &codex_archive_entry("1.0.0", &sha256_hex(b"unused"), vec![]),
        &status,
    ));
}

#[test]
fn managed_dependency_update_unavailable_for_container_provider_with_matching_dual_node_runtimes() {
    let mut cfg = AgentServerConfigFile::default();
    cfg.managed_provider_targets.insert(
        "amp".to_string(),
        HashMap::from([(
            InstallTarget::Container.as_str().to_string(),
            AgentServerCommand {
                command: "/tmp/amp-acp.js".to_string(),
                args: Vec::new(),
                dependencies: vec![
                    "runtime-node-container".to_string(),
                    "runtime-node-host".to_string(),
                ],
                managed: Some(ManagedInstallMetadata {
                    package: Some("https://example.invalid/amp.tar.gz".to_string()),
                    version: Some("0.1.2".to_string()),
                    artifact_fingerprint: None,
                    archive_sha256: None,
                    target: Some(InstallTarget::Container),
                    install_dir_rel: None,
                    bin_dir_rel: None,
                    last_success_at: None,
                    last_error: None,
                }),
            },
        )]),
    );
    let expected = crate::expected_managed_dependency_version("runtime-node-host")
        .expect("runtime node version");
    let expected_host_fingerprint = crate::expected_managed_dependency_artifact_fingerprint_for_id(
        "runtime-node-host",
        None,
        InstallTarget::Host,
    )
    .expect("host runtime node fingerprint");
    let expected_container_fingerprint =
        crate::expected_managed_dependency_artifact_fingerprint_for_id(
            "runtime-node-container",
            None,
            InstallTarget::Container,
        )
        .expect("container runtime node fingerprint");
    cfg.managed_installs.insert(
        "runtime-node-host".to_string(),
        ManagedInstallMetadata {
            package: Some("node-runtime".to_string()),
            version: Some(expected.to_string()),
            artifact_fingerprint: Some(expected_host_fingerprint),
            archive_sha256: None,
            target: Some(InstallTarget::Host),
            install_dir_rel: None,
            bin_dir_rel: None,
            last_success_at: None,
            last_error: None,
        },
    );
    cfg.managed_installs.insert(
        "runtime-node-container".to_string(),
        ManagedInstallMetadata {
            package: Some("node-runtime".to_string()),
            version: Some(expected.to_string()),
            artifact_fingerprint: Some(expected_container_fingerprint),
            archive_sha256: None,
            target: Some(InstallTarget::Container),
            install_dir_rel: None,
            bin_dir_rel: None,
            last_success_at: None,
            last_error: None,
        },
    );
    let status = installed_status("amp", InstallTarget::Container);
    let entry = ProviderMatrixEntry {
        id: "amp".to_string(),
        kind: ProviderMatrixEntryKind::Harness,
        display_name: Some("Amp".to_string()),
        tier: Some("tier2".to_string()),
        command: None,
        managed_install: Some(ProviderInstall::Archive {
            version: "0.1.2".to_string(),
            args: Vec::new(),
            targets: HashMap::from([(
                "linux-aarch64".to_string(),
                ProviderArchiveTarget {
                    url: "https://example.invalid/amp.tar.gz".to_string(),
                    sha256: None,
                    size_bytes: None,
                    archive: ProviderArchiveKind::None,
                    bin_path: "dist/bin/amp-acp.js".to_string(),
                },
            )]),
        }),
        provider_dependencies: Vec::new(),
        dependencies: Vec::new(),
        version_probe: None,
        releases: vec![release("0.1.2", ProviderReleaseStatus::Supported, None)],
    };

    assert!(!managed_dependency_update_available(&cfg, &entry, &status));
}

#[tokio::test]
async fn provider_status_matrix_uses_target_scoped_dependency_metadata() {
    let temp = tempfile::tempdir().expect("tempdir");
    let target = InstallTarget::LinuxX8664;
    let target_key = target.as_str().to_string();
    let provider_sha = sha256_hex(b"provider-archive");
    let dependency_sha = sha256_hex(b"dependency-archive");
    let entry = ProviderMatrixEntry {
        id: "targeted-provider".to_string(),
        kind: ProviderMatrixEntryKind::Harness,
        display_name: Some("Targeted Provider".to_string()),
        tier: Some("tier2".to_string()),
        command: None,
        managed_install: Some(ProviderInstall::Archive {
            version: "1.0.0".to_string(),
            args: Vec::new(),
            targets: HashMap::from([(
                target_key.clone(),
                ProviderArchiveTarget {
                    url: "https://example.invalid/targeted-provider.tar.gz".to_string(),
                    sha256: Some(provider_sha.clone()),
                    size_bytes: None,
                    archive: ProviderArchiveKind::TarGz,
                    bin_path: "targeted-provider".to_string(),
                },
            )]),
        }),
        provider_dependencies: Vec::new(),
        dependencies: vec![ProviderDependency {
            id: "targeted-dependency".to_string(),
            install: DependencyInstall::Archive {
                version: "2.0.0".to_string(),
                targets: HashMap::from([(
                    target_key.clone(),
                    ProviderArchiveTarget {
                        url: "https://example.invalid/targeted-dependency.tar.gz".to_string(),
                        sha256: Some(dependency_sha.clone()),
                        size_bytes: None,
                        archive: ProviderArchiveKind::TarGz,
                        bin_path: "targeted-dependency".to_string(),
                    },
                )]),
            },
        }],
        version_probe: None,
        releases: vec![release("1.0.0", ProviderReleaseStatus::Supported, None)],
    };
    let provider_meta = ManagedInstallMetadata {
        package: Some("https://example.invalid/targeted-provider.tar.gz".to_string()),
        version: Some("1.0.0".to_string()),
        artifact_fingerprint: Some(provider_sha.clone()),
        archive_sha256: Some(provider_sha),
        target: Some(target),
        install_dir_rel: Some("providers/agent-servers/targeted-provider/1.0.0".to_string()),
        bin_dir_rel: None,
        last_success_at: None,
        last_error: None,
    };
    let mut cfg = AgentServerConfigFile::default();
    cfg.managed_provider_targets.insert(
        "targeted-provider".to_string(),
        HashMap::from([(
            target_key.clone(),
            AgentServerCommand {
                command: "/tmp/targeted-provider".to_string(),
                args: Vec::new(),
                dependencies: vec!["targeted-dependency".to_string()],
                managed: Some(provider_meta.clone()),
            },
        )]),
    );
    cfg.managed_install_targets.insert(
        "targeted-provider".to_string(),
        HashMap::from([(target_key.clone(), provider_meta)]),
    );
    cfg.managed_install_targets.insert(
        "targeted-dependency".to_string(),
        HashMap::from([(
            target_key.clone(),
            ManagedInstallMetadata {
                package: Some("https://example.invalid/targeted-dependency.tar.gz".to_string()),
                version: Some("2.0.0".to_string()),
                artifact_fingerprint: Some(dependency_sha.clone()),
                archive_sha256: Some(dependency_sha),
                target: Some(target),
                install_dir_rel: Some(
                    "providers/agent-servers/targeted-dependency/2.0.0".to_string(),
                ),
                bin_dir_rel: None,
                last_success_at: None,
                last_error: None,
            },
        )]),
    );
    let mut status = installed_status("targeted-provider", target);
    status.version = Some("1.0.0".to_string());

    apply_matrix_to_status(temp.path(), &cfg, &entry, &mut status, CURRENT_CTX_VERSION).await;

    assert!(
        !status
            .details
            .contains_key("managed_dependency_update_available"),
        "matching target-scoped dependency metadata must not force a reinstall"
    );
    assert!(!status.details.contains_key("matrix_update_available"));
}

#[tokio::test]
async fn provider_status_matrix_flags_managed_archive_checksum_mismatch() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = temp.path().join("codex");
    std::fs::write(&runtime, b"runtime-bytes-do-not-matter").expect("write runtime");

    let expected_sha256 = sha256_hex(b"new-codex-runtime");
    let actual_sha256 = sha256_hex(b"previous-archive");
    let entry = codex_archive_entry(
        "1.0.0",
        &expected_sha256,
        vec![release("1.0.0", ProviderReleaseStatus::Supported, None)],
    );
    let cfg = managed_archive_cfg(&runtime, "1.0.0", &actual_sha256);
    let mut status = installed_status("codex", InstallTarget::LinuxX8664);
    status.capabilities = Some(ctx_providers::adapters::ProviderCapabilities {
        stream_events: true,
        stream_format: "jsonl".to_string(),
        has_turn_boundaries: true,
        has_tool_call_ids: true,
        has_file_change_events: true,
        has_command_events: true,
        supports_resume: true,
        supports_stable_session_id: true,
        supports_fork_or_rewind: true,
        supports_headless: true,
        supports_server_mode: true,
        supports_interactive_tui: false,
        supports_private_state_dir: true,
        supports_sandbox_flags: true,
        supports_approval_flags: true,
        notes: Vec::new(),
    });

    apply_matrix_to_status(temp.path(), &cfg, &entry, &mut status, CURRENT_CTX_VERSION).await;

    assert!(!status.installed);
    assert!(status.capabilities.is_none());
    assert!(matches!(status.health, ProviderHealth::Error));
    assert_eq!(
        status
            .details
            .get("managed_checksum_mismatch")
            .map(String::as_str),
        Some("true")
    );
    assert_eq!(
        status
            .details
            .get("managed_expected_archive_sha256")
            .map(String::as_str),
        Some(expected_sha256.as_str())
    );
    assert_eq!(
        status
            .details
            .get("managed_detected_archive_sha256")
            .map(String::as_str),
        Some(actual_sha256.as_str())
    );
    assert!(status
        .diagnostics
        .iter()
        .any(|message| message.contains("checksum mismatch")));
}

#[tokio::test]
async fn provider_status_matrix_accepts_matching_managed_archive_checksum() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = temp.path().join("codex");
    std::fs::write(&runtime, b"bridge-or-runtime-bytes-can-differ").expect("write runtime");

    let sha256 = sha256_hex(b"matching-downloaded-archive");
    let entry = codex_archive_entry(
        "1.0.0",
        &sha256,
        vec![release("1.0.0", ProviderReleaseStatus::Supported, None)],
    );
    let cfg = managed_archive_cfg(&runtime, "1.0.0", &sha256);
    let mut status = installed_status("codex", InstallTarget::LinuxX8664);

    apply_matrix_to_status(temp.path(), &cfg, &entry, &mut status, CURRENT_CTX_VERSION).await;

    assert!(status.installed);
    assert!(!status.details.contains_key("managed_checksum_mismatch"));
    assert!(status
        .diagnostics
        .iter()
        .all(|message| !message.contains("checksum mismatch")));
}

#[tokio::test]
async fn provider_status_matrix_clears_stale_matrix_update_flags_when_runtime_is_current() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = temp.path().join("codex");
    std::fs::write(&runtime, b"matching-runtime").expect("write runtime");

    let sha256 = sha256_hex(b"matching-archive");
    let entry = codex_archive_entry(
        "1.0.0",
        &sha256,
        vec![release("1.0.0", ProviderReleaseStatus::Supported, None)],
    );
    let cfg = managed_archive_cfg(&runtime, "1.0.0", &sha256);
    let mut status = installed_status("codex", InstallTarget::LinuxX8664);
    status.version = Some("0.9.0".to_string());
    status.details.extend(HashMap::from([
        (
            "managed_dependency_update_available".to_string(),
            "true".to_string(),
        ),
        (
            "managed_fingerprint_mismatch".to_string(),
            "true".to_string(),
        ),
        (
            "matrix_detected_upstream_version".to_string(),
            "0.9.0".to_string(),
        ),
        ("matrix_latest_version".to_string(), "9.9.9".to_string()),
        (
            "matrix_recommended_version".to_string(),
            "9.9.9".to_string(),
        ),
        ("matrix_update_available".to_string(), "true".to_string()),
        (
            "matrix_update_requires_context".to_string(),
            "true".to_string(),
        ),
    ]));

    apply_matrix_to_status(temp.path(), &cfg, &entry, &mut status, CURRENT_CTX_VERSION).await;

    assert_eq!(status.version.as_deref(), Some("1.0.0"));
    assert_eq!(
        status
            .details
            .get("matrix_recommended_version")
            .map(String::as_str),
        Some("1.0.0")
    );
    assert!(!status
        .details
        .contains_key("managed_dependency_update_available"));
    assert!(!status.details.contains_key("managed_fingerprint_mismatch"));
    assert!(!status.details.contains_key("matrix_update_available"));
    assert!(!status
        .details
        .contains_key("matrix_update_requires_context"));
}

#[tokio::test]
async fn provider_status_matrix_marks_stale_installed_provider_as_updateable_for_current_ctx() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = temp.path().join("codex");
    std::fs::write(&runtime, b"matching-runtime").expect("write runtime");

    let sha_old = sha256_hex(b"old-archive");
    let sha_new = sha256_hex(b"new-archive");
    let entry = codex_archive_entry(
        "1.0.1",
        &sha_new,
        vec![release(
            "1.0.1",
            ProviderReleaseStatus::Supported,
            Some("0.59.0"),
        )],
    );
    let cfg = managed_archive_cfg(&runtime, "1.0.0", &sha_old);
    let mut status = installed_status("codex", InstallTarget::LinuxX8664);

    apply_matrix_to_status(temp.path(), &cfg, &entry, &mut status, CURRENT_CTX_VERSION).await;

    assert_eq!(
        status
            .details
            .get("matrix_recommended_version")
            .map(String::as_str),
        Some("1.0.1")
    );
    assert_eq!(
        status
            .details
            .get("matrix_update_available")
            .map(String::as_str),
        Some("true")
    );
    assert!(matches!(status.health, ProviderHealth::UnsupportedVersion));
}

#[tokio::test]
async fn provider_status_matrix_marks_out_of_matrix_runtime_as_unsupported() {
    let temp = tempfile::tempdir().expect("tempdir");
    let entry = codex_npm_entry(vec![release(
        "1.2.3",
        ProviderReleaseStatus::Supported,
        None,
    )]);
    let mut cfg = AgentServerConfigFile::default();
    cfg.managed_install_targets.insert(
        "codex".to_string(),
        HashMap::from([(
            InstallTarget::Host.as_str().to_string(),
            ManagedInstallMetadata {
                package: Some("@openai/codex".to_string()),
                version: Some("0.9.0".to_string()),
                artifact_fingerprint: Some("npm:@openai/codex@0.9.0".to_string()),
                archive_sha256: None,
                target: Some(InstallTarget::Host),
                install_dir_rel: Some("providers/agent-servers/codex/0.9.0".to_string()),
                bin_dir_rel: Some("providers/agent-servers/codex/0.9.0/bin".to_string()),
                last_success_at: None,
                last_error: None,
            },
        )]),
    );
    let mut status = installed_status("codex", InstallTarget::Host);

    apply_matrix_to_status(temp.path(), &cfg, &entry, &mut status, CURRENT_CTX_VERSION).await;

    assert!(matches!(status.health, ProviderHealth::UnsupportedVersion));
    assert!(status
        .diagnostics
        .iter()
        .any(|message| message.contains("not in the support matrix")));
}

#[tokio::test]
async fn provider_status_matrix_flags_missing_npm_artifact_fingerprint() {
    let entry = codex_npm_entry(vec![release(
        "1.2.3",
        ProviderReleaseStatus::Supported,
        None,
    )]);
    let mut cfg = AgentServerConfigFile::default();
    let meta = ManagedInstallMetadata {
        package: Some("@openai/codex".to_string()),
        version: Some("1.2.3".to_string()),
        artifact_fingerprint: None,
        archive_sha256: None,
        target: Some(InstallTarget::Host),
        install_dir_rel: Some("providers/agent-servers/codex/1.2.3".to_string()),
        bin_dir_rel: None,
        last_success_at: None,
        last_error: None,
    };
    cfg.managed_install_targets.insert(
        "codex".to_string(),
        HashMap::from([(InstallTarget::Host.as_str().to_string(), meta.clone())]),
    );
    cfg.managed_provider_targets.insert(
        "codex".to_string(),
        HashMap::from([(
            InstallTarget::Host.as_str().to_string(),
            AgentServerCommand {
                command: "/tmp/codex".to_string(),
                args: Vec::new(),
                dependencies: Vec::new(),
                managed: Some(meta),
            },
        )]),
    );
    let mut status = installed_status("codex", InstallTarget::Host);

    apply_matrix_to_status(
        Path::new("/tmp"),
        &cfg,
        &entry,
        &mut status,
        CURRENT_CTX_VERSION,
    )
    .await;

    assert!(!status.installed);
    assert_eq!(
        status
            .details
            .get("managed_fingerprint_mismatch")
            .map(String::as_str),
        Some("true")
    );
    assert_eq!(
        status
            .details
            .get("managed_detected_fingerprint")
            .map(String::as_str),
        Some("<missing>")
    );
}
