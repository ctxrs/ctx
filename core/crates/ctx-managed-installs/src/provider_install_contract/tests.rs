use super::*;
use std::collections::HashMap;

use crate::AgentServerCommand;
use ctx_provider_matrix::{
    ProviderArchiveKind, ProviderArchiveTarget, ProviderInstall, ProviderMatrixEntry,
    ProviderMatrixEntryKind, ProviderRelease, ProviderReleaseStatus,
};

const TEST_CTX_VERSION: Option<&str> = Some("0.59.0");

fn matrix_with_entries(entries: Vec<ProviderMatrixEntry>) -> ProviderMatrix {
    ProviderMatrix {
        version: 2,
        generated_at: None,
        providers: entries,
    }
}

fn env_lock() -> &'static tokio::sync::Mutex<()> {
    crate::test_support::process_env_test_lock()
}

fn archive_entry(provider_id: &str, kind: ProviderMatrixEntryKind) -> ProviderMatrixEntry {
    let mut targets = HashMap::from([
        (
            "linux-x86_64".to_string(),
            ProviderArchiveTarget {
                url: "https://example.invalid/provider-x86_64.tar.gz".to_string(),
                sha256: None,
                size_bytes: None,
                archive: ProviderArchiveKind::TarGz,
                bin_path: "provider".to_string(),
            },
        ),
        (
            "linux-aarch64".to_string(),
            ProviderArchiveTarget {
                url: "https://example.invalid/provider-aarch64.tar.gz".to_string(),
                sha256: None,
                size_bytes: None,
                archive: ProviderArchiveKind::TarGz,
                bin_path: "provider".to_string(),
            },
        ),
    ]);
    if let Ok(host_target_key) = installer::resolve_matrix_target_key(InstallTarget::Host) {
        targets.insert(
            host_target_key.to_string(),
            ProviderArchiveTarget {
                url: "https://example.invalid/provider-host.tar.gz".to_string(),
                sha256: None,
                size_bytes: None,
                archive: ProviderArchiveKind::TarGz,
                bin_path: "provider".to_string(),
            },
        );
    }
    ProviderMatrixEntry {
        id: provider_id.to_string(),
        kind,
        display_name: None,
        tier: None,
        command: None,
        managed_install: Some(ProviderInstall::Archive {
            version: "1.0.0".to_string(),
            args: Vec::new(),
            targets,
        }),
        provider_dependencies: Vec::new(),
        dependencies: Vec::new(),
        version_probe: None,
        releases: vec![ProviderRelease {
            version: "1.0.0".to_string(),
            status: ProviderReleaseStatus::Supported,
            upstream_version: None,
            provenance: None,
            context_min: None,
            context_max: None,
            notes: None,
        }],
    }
}

fn npm_entry(
    provider_id: &str,
    kind: ProviderMatrixEntryKind,
    package: &str,
) -> ProviderMatrixEntry {
    ProviderMatrixEntry {
        id: provider_id.to_string(),
        kind,
        display_name: None,
        tier: None,
        command: None,
        managed_install: Some(ProviderInstall::Npm {
            package: package.to_string(),
            version: "1.0.0".to_string(),
            entrypoint: "cli.js".to_string(),
            args: Vec::new(),
            targets: std::collections::HashMap::new(),
        }),
        provider_dependencies: Vec::new(),
        dependencies: Vec::new(),
        version_probe: None,
        releases: vec![ProviderRelease {
            version: "1.0.0".to_string(),
            status: ProviderReleaseStatus::Supported,
            upstream_version: None,
            provenance: None,
            context_min: None,
            context_max: None,
            notes: None,
        }],
    }
}

#[test]
fn acp_provider_requires_bridge_runtime() {
    let _guard = env_lock().blocking_lock();
    let root = tempfile::tempdir().expect("tempdir");
    let cfg = AgentServerConfigFile::default();
    let err = resolve_provider_install_contract(
        root.path(),
        &cfg,
        &matrix_with_entries(vec![archive_entry(
            "kimi",
            ProviderMatrixEntryKind::Harness,
        )]),
        "kimi",
        InstallTarget::Container,
        TEST_CTX_VERSION,
    )
    .expect_err("missing bridge should block ACP install");
    assert_eq!(
        err,
        ProviderInstallViabilityIssue {
            code: "acp_bridge_missing",
            message: "ACP bridge runtime is not viable for target 'container': runtime command is not configured for provider 'acp-crp-bridge' required by provider 'kimi'".to_string(),
        }
    );
}

#[test]
fn acp_container_install_plans_bridge_prerequisite_when_bridge_is_installable() {
    let _guard = env_lock().blocking_lock();
    let root = tempfile::tempdir().expect("tempdir");
    let cfg = AgentServerConfigFile::default();
    let contract = resolve_provider_install_contract(
        root.path(),
        &cfg,
        &matrix_with_entries(vec![
            archive_entry("kimi", ProviderMatrixEntryKind::Harness),
            archive_entry("acp-crp-bridge", ProviderMatrixEntryKind::Dependency),
        ]),
        "kimi",
        InstallTarget::Container,
        TEST_CTX_VERSION,
    )
    .expect("missing installable bridge should become a prerequisite");

    assert_eq!(
        contract.dependencies_for_role(ProviderInstallDependencyRoleKind::Prerequisite),
        vec![ProviderInstallDependency {
            provider_id: "acp-crp-bridge".to_string(),
            role: ProviderInstallDependencyRoleKind::Prerequisite,
            target: InstallTarget::Container,
            satisfied: false,
        }]
    );
}

#[test]
fn acp_host_install_plans_bridge_prerequisite_when_bridge_is_installable() {
    let _guard = env_lock().blocking_lock();
    let root = tempfile::tempdir().expect("tempdir");
    let cfg = AgentServerConfigFile::default();
    let contract = resolve_provider_install_contract(
        root.path(),
        &cfg,
        &matrix_with_entries(vec![
            archive_entry("kimi", ProviderMatrixEntryKind::Harness),
            archive_entry("acp-crp-bridge", ProviderMatrixEntryKind::Dependency),
        ]),
        "kimi",
        InstallTarget::Host,
        TEST_CTX_VERSION,
    )
    .expect("missing installable host bridge should become a prerequisite");

    assert_eq!(
        contract.dependencies_for_role(ProviderInstallDependencyRoleKind::Prerequisite),
        vec![ProviderInstallDependency {
            provider_id: "acp-crp-bridge".to_string(),
            role: ProviderInstallDependencyRoleKind::Prerequisite,
            target: InstallTarget::Host,
            satisfied: false,
        }]
    );
}

#[test]
fn native_provider_does_not_require_bridge_runtime() {
    let _guard = env_lock().blocking_lock();
    let root = tempfile::tempdir().expect("tempdir");
    let cfg = AgentServerConfigFile::default();
    let contract = resolve_provider_install_contract(
        root.path(),
        &cfg,
        &matrix_with_entries(vec![archive_entry(
            "codex",
            ProviderMatrixEntryKind::Harness,
        )]),
        "codex",
        InstallTarget::Container,
        TEST_CTX_VERSION,
    )
    .expect("native provider should be viable without ACP bridge");
    assert!(contract.dependencies.is_empty());
    assert!(matches!(
        contract.resolved_target_key,
        "linux-x86_64" | "linux-aarch64"
    ));
}

#[test]
fn provider_install_contract_respects_current_ctx_version() {
    let _guard = env_lock().blocking_lock();
    let root = tempfile::tempdir().expect("tempdir");
    let cfg = AgentServerConfigFile::default();
    let mut codex = archive_entry("codex", ProviderMatrixEntryKind::Harness);
    codex.releases = vec![ProviderRelease {
        version: "0.114.0-ctx.5".to_string(),
        status: ProviderReleaseStatus::Supported,
        upstream_version: Some("0.114.0".to_string()),
        provenance: None,
        context_min: Some("0.59.0".to_string()),
        context_max: None,
        notes: None,
    }];

    let err = resolve_provider_install_contract(
        root.path(),
        &cfg,
        &matrix_with_entries(vec![codex]),
        "codex",
        InstallTarget::Host,
        Some("0.58.9"),
    )
    .expect_err("older ctx builds must not treat newer managed releases as installable");

    assert_eq!(err.code, "ctx_version_unsupported");
}

#[test]
fn provider_install_contract_requires_current_ctx_version() {
    let _guard = env_lock().blocking_lock();
    let root = tempfile::tempdir().expect("tempdir");
    let cfg = AgentServerConfigFile::default();

    let err = resolve_provider_install_contract(
        root.path(),
        &cfg,
        &matrix_with_entries(vec![archive_entry(
            "codex",
            ProviderMatrixEntryKind::Harness,
        )]),
        "codex",
        InstallTarget::Host,
        None,
    )
    .expect_err("provider install resolution must fail closed without build identity");

    assert_eq!(err.code, "ctx_version_unavailable");
}

#[test]
fn hybrid_provider_without_container_artifact_fails_closed() {
    let _guard = env_lock().blocking_lock();
    let root = tempfile::tempdir().expect("tempdir");
    let cfg = AgentServerConfigFile::default();

    let issue = provider_install_viability_issue(
        root.path(),
        &cfg,
        &matrix_with_entries(vec![npm_entry(
            "gemini",
            ProviderMatrixEntryKind::Harness,
            "@google/gemini-cli",
        )]),
        "gemini",
        InstallTarget::Container,
        TEST_CTX_VERSION,
    )
    .expect("container installs without staged artifacts must fail closed");

    assert_eq!(issue.code, "container_artifact_missing");
    assert!(issue.message.contains("published managed artifact"));
}

#[test]
fn hybrid_provider_with_container_artifact_stays_installable() {
    let _guard = env_lock().blocking_lock();
    let root = tempfile::tempdir().expect("tempdir");
    let cfg = AgentServerConfigFile::default();
    let mut gemini = npm_entry(
        "gemini",
        ProviderMatrixEntryKind::Harness,
        "@google/gemini-cli",
    );
    if let Some(ProviderInstall::Npm { targets, .. }) = gemini.managed_install.as_mut() {
        targets.insert(
            installer::resolve_matrix_target_key(InstallTarget::Container)
                .expect("container target key")
                .to_string(),
            ProviderArchiveTarget {
                url: "https://example.invalid/gemini-container.tar.gz".to_string(),
                sha256: Some("a".repeat(64)),
                size_bytes: None,
                archive: ProviderArchiveKind::TarGz,
                bin_path: "gemini".to_string(),
            },
        );
    }

    let contract = resolve_provider_install_contract(
        root.path(),
        &cfg,
        &matrix_with_entries(vec![
            gemini,
            archive_entry("acp-crp-bridge", ProviderMatrixEntryKind::Dependency),
        ]),
        "gemini",
        InstallTarget::Container,
        TEST_CTX_VERSION,
    )
    .expect("staged hybrid providers should remain installable for container targets");

    assert_eq!(
        contract.dependencies_for_role(ProviderInstallDependencyRoleKind::Prerequisite),
        vec![ProviderInstallDependency {
            provider_id: "acp-crp-bridge".to_string(),
            role: ProviderInstallDependencyRoleKind::Prerequisite,
            target: InstallTarget::Container,
            satisfied: false,
        }]
    );
}

#[test]
fn invalid_managed_bridge_runtime_stays_repairable() {
    let _guard = env_lock().blocking_lock();
    let root = tempfile::tempdir().expect("tempdir");
    let mut cfg = AgentServerConfigFile::default();
    cfg.managed_provider_targets.insert(
        "acp-crp-bridge".to_string(),
        HashMap::from([(
            "container".to_string(),
            AgentServerCommand {
                command: "relative-bridge".to_string(),
                args: Vec::new(),
                dependencies: Vec::new(),
                managed: None,
            },
        )]),
    );
    let contract = resolve_provider_install_contract(
        root.path(),
        &cfg,
        &matrix_with_entries(vec![
            archive_entry("kimi", ProviderMatrixEntryKind::Harness),
            archive_entry("acp-crp-bridge", ProviderMatrixEntryKind::Dependency),
        ]),
        "kimi",
        InstallTarget::Container,
        TEST_CTX_VERSION,
    )
    .expect("stale managed bridge should stay repairable");
    assert_eq!(
        contract.dependencies_for_role(ProviderInstallDependencyRoleKind::Prerequisite),
        vec![ProviderInstallDependency {
            provider_id: "acp-crp-bridge".to_string(),
            role: ProviderInstallDependencyRoleKind::Prerequisite,
            target: InstallTarget::Container,
            satisfied: false,
        }]
    );
}

#[test]
fn invalid_user_override_bridge_runtime_is_reported() {
    let _guard = env_lock().blocking_lock();
    let root = tempfile::tempdir().expect("tempdir");
    let mut cfg = AgentServerConfigFile::default();
    cfg.providers.insert(
        "acp-crp-bridge".to_string(),
        AgentServerCommand {
            command: "relative-bridge".to_string(),
            args: Vec::new(),
            dependencies: Vec::new(),
            managed: None,
        },
    );
    let issue = provider_install_viability_issue(
        root.path(),
        &cfg,
        &matrix_with_entries(vec![
            archive_entry("kimi", ProviderMatrixEntryKind::Harness),
            archive_entry("acp-crp-bridge", ProviderMatrixEntryKind::Dependency),
        ]),
        "kimi",
        InstallTarget::Container,
        TEST_CTX_VERSION,
    )
    .expect("invalid user bridge override should still be reported");
    assert_eq!(issue.code, "acp_bridge_invalid");
    assert!(issue.message.contains("relative-bridge"));
}

#[test]
fn claude_install_resolves_host_readiness_dependency() {
    let _guard = env_lock().blocking_lock();
    let root = tempfile::tempdir().expect("tempdir");
    let cfg = AgentServerConfigFile::default();
    let mut claude = archive_entry("claude-crp", ProviderMatrixEntryKind::Harness);
    claude.provider_dependencies = vec![provider_matrix::ProviderInstallDependency {
        id: "claude-cli".to_string(),
        role: ProviderInstallDependencyRole::Readiness,
        target: ProviderInstallDependencyTarget::Host,
    }];
    let contract = resolve_provider_install_contract(
        root.path(),
        &cfg,
        &matrix_with_entries(vec![
            claude,
            npm_entry(
                "claude-cli",
                ProviderMatrixEntryKind::Dependency,
                "@anthropic-ai/claude-code",
            ),
        ]),
        "claude-crp",
        InstallTarget::Container,
        TEST_CTX_VERSION,
    )
    .expect("claude should plan host claude-cli readiness dependency");

    assert_eq!(
        contract.dependencies_for_role(ProviderInstallDependencyRoleKind::Readiness),
        vec![ProviderInstallDependency {
            provider_id: "claude-cli".to_string(),
            role: ProviderInstallDependencyRoleKind::Readiness,
            target: InstallTarget::Host,
            satisfied: false,
        }]
    );
}

#[test]
fn claude_readiness_dependency_is_marked_satisfied_when_configured() {
    let _guard = env_lock().blocking_lock();
    let root = tempfile::tempdir().expect("tempdir");
    let script_path = root.path().join("claude");
    std::fs::write(&script_path, "#!/bin/sh\nexit 0\n").expect("write script");
    let mut perms = std::fs::metadata(&script_path)
        .expect("metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).expect("set perms");
    }
    let mut cfg = AgentServerConfigFile::default();
    cfg.providers.insert(
        "claude-cli".to_string(),
        AgentServerCommand {
            command: script_path.to_string_lossy().to_string(),
            args: Vec::new(),
            dependencies: Vec::new(),
            managed: None,
        },
    );
    let mut claude = archive_entry("claude-crp", ProviderMatrixEntryKind::Harness);
    claude.provider_dependencies = vec![provider_matrix::ProviderInstallDependency {
        id: "claude-cli".to_string(),
        role: ProviderInstallDependencyRole::Readiness,
        target: ProviderInstallDependencyTarget::Host,
    }];
    let contract = resolve_provider_install_contract(
        root.path(),
        &cfg,
        &matrix_with_entries(vec![
            claude,
            npm_entry(
                "claude-cli",
                ProviderMatrixEntryKind::Dependency,
                "@anthropic-ai/claude-code",
            ),
        ]),
        "claude-crp",
        InstallTarget::Host,
        TEST_CTX_VERSION,
    )
    .expect("configured claude-cli should satisfy readiness dependency");

    assert_eq!(
        contract.dependencies_for_role(ProviderInstallDependencyRoleKind::Readiness),
        vec![ProviderInstallDependency {
            provider_id: "claude-cli".to_string(),
            role: ProviderInstallDependencyRoleKind::Readiness,
            target: InstallTarget::Host,
            satisfied: true,
        }]
    );
}

#[test]
fn codex_install_resolves_same_target_prerequisite_dependency() {
    let _guard = env_lock().blocking_lock();
    let root = tempfile::tempdir().expect("tempdir");
    let cfg = AgentServerConfigFile::default();
    let mut codex = archive_entry("codex", ProviderMatrixEntryKind::Harness);
    codex.provider_dependencies = vec![provider_matrix::ProviderInstallDependency {
        id: "codex-cli".to_string(),
        role: ProviderInstallDependencyRole::Prerequisite,
        target: ProviderInstallDependencyTarget::SameAsProvider,
    }];
    let contract = resolve_provider_install_contract(
        root.path(),
        &cfg,
        &matrix_with_entries(vec![
            codex,
            archive_entry("codex-cli", ProviderMatrixEntryKind::Dependency),
        ]),
        "codex",
        InstallTarget::Container,
        TEST_CTX_VERSION,
    )
    .expect("codex should plan container codex-cli prerequisite dependency");

    assert_eq!(
        contract.dependencies_for_role(ProviderInstallDependencyRoleKind::Prerequisite),
        vec![ProviderInstallDependency {
            provider_id: "codex-cli".to_string(),
            role: ProviderInstallDependencyRoleKind::Prerequisite,
            target: InstallTarget::Container,
            satisfied: false,
        }]
    );
}

#[test]
fn codex_prerequisite_dependency_is_marked_satisfied_when_configured() {
    let _guard = env_lock().blocking_lock();
    let root = tempfile::tempdir().expect("tempdir");
    let script_path = root.path().join("codex");
    std::fs::write(&script_path, "#!/bin/sh\nexit 0\n").expect("write script");
    let mut perms = std::fs::metadata(&script_path)
        .expect("metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).expect("set perms");
    }
    let mut cfg = AgentServerConfigFile::default();
    cfg.providers.insert(
        "codex-cli".to_string(),
        AgentServerCommand {
            command: script_path.to_string_lossy().to_string(),
            args: Vec::new(),
            dependencies: Vec::new(),
            managed: None,
        },
    );
    let mut codex = archive_entry("codex", ProviderMatrixEntryKind::Harness);
    codex.provider_dependencies = vec![provider_matrix::ProviderInstallDependency {
        id: "codex-cli".to_string(),
        role: ProviderInstallDependencyRole::Prerequisite,
        target: ProviderInstallDependencyTarget::SameAsProvider,
    }];
    let contract = resolve_provider_install_contract(
        root.path(),
        &cfg,
        &matrix_with_entries(vec![
            codex,
            archive_entry("codex-cli", ProviderMatrixEntryKind::Dependency),
        ]),
        "codex",
        InstallTarget::Host,
        TEST_CTX_VERSION,
    )
    .expect("configured codex-cli should satisfy prerequisite dependency");

    assert_eq!(
        contract.dependencies_for_role(ProviderInstallDependencyRoleKind::Prerequisite),
        vec![ProviderInstallDependency {
            provider_id: "codex-cli".to_string(),
            role: ProviderInstallDependencyRoleKind::Prerequisite,
            target: InstallTarget::Host,
            satisfied: true,
        }]
    );
}

#[test]
fn codex_container_prerequisite_dependency_is_marked_satisfied_when_configured() {
    let _guard = env_lock().blocking_lock();
    let root = tempfile::tempdir().expect("tempdir");
    let script_path = root.path().join("codex-linux");
    std::fs::write(&script_path, "#!/bin/sh\nexit 0\n").expect("write script");
    let mut perms = std::fs::metadata(&script_path)
        .expect("metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).expect("set perms");
    }
    let mut cfg = AgentServerConfigFile::default();
    cfg.managed_provider_targets.insert(
        "codex-cli".to_string(),
        HashMap::from([(
            "container".to_string(),
            AgentServerCommand {
                command: script_path.to_string_lossy().to_string(),
                args: Vec::new(),
                dependencies: Vec::new(),
                managed: None,
            },
        )]),
    );
    let mut codex = archive_entry("codex", ProviderMatrixEntryKind::Harness);
    codex.provider_dependencies = vec![provider_matrix::ProviderInstallDependency {
        id: "codex-cli".to_string(),
        role: ProviderInstallDependencyRole::Prerequisite,
        target: ProviderInstallDependencyTarget::SameAsProvider,
    }];
    let contract = resolve_provider_install_contract(
        root.path(),
        &cfg,
        &matrix_with_entries(vec![
            codex,
            archive_entry("codex-cli", ProviderMatrixEntryKind::Dependency),
        ]),
        "codex",
        InstallTarget::Container,
        TEST_CTX_VERSION,
    )
    .expect("configured container codex-cli should satisfy prerequisite dependency");

    assert_eq!(
        contract.dependencies_for_role(ProviderInstallDependencyRoleKind::Prerequisite),
        vec![ProviderInstallDependency {
            provider_id: "codex-cli".to_string(),
            role: ProviderInstallDependencyRoleKind::Prerequisite,
            target: InstallTarget::Container,
            satisfied: true,
        }]
    );
}
