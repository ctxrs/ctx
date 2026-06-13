use super::*;

#[test]
fn codex_env_injects_target_specific_codex_cli_command_path() {
    let tmp = tempdir().expect("tempdir");
    let host_codex = tmp.path().join("codex-host");
    let container_codex = tmp.path().join("codex-container");
    std::fs::write(&host_codex, b"#!/bin/sh\n").expect("write host codex");
    std::fs::write(&container_codex, b"#!/bin/sh\n").expect("write container codex");

    let mut cfg = AgentServerConfigFile::default();
    cfg.managed_provider_targets.insert(
        "codex-cli".to_string(),
        HashMap::from([
            (
                InstallTarget::Host.as_str().to_string(),
                AgentServerCommand {
                    command: host_codex.to_string_lossy().to_string(),
                    args: Vec::new(),
                    dependencies: Vec::new(),
                    managed: Some(ManagedInstallMetadata {
                        package: None,
                        version: None,
                        artifact_fingerprint: None,
                        archive_sha256: None,
                        target: Some(InstallTarget::Host),
                        install_dir_rel: None,
                        bin_dir_rel: None,
                        last_success_at: None,
                        last_error: None,
                    }),
                },
            ),
            (
                InstallTarget::Container.as_str().to_string(),
                AgentServerCommand {
                    command: container_codex.to_string_lossy().to_string(),
                    args: Vec::new(),
                    dependencies: Vec::new(),
                    managed: Some(ManagedInstallMetadata {
                        package: None,
                        version: None,
                        artifact_fingerprint: None,
                        archive_sha256: None,
                        target: Some(InstallTarget::Container),
                        install_dir_rel: None,
                        bin_dir_rel: None,
                        last_success_at: None,
                        last_error: None,
                    }),
                },
            ),
        ]),
    );

    let mut provider_env = HashMap::new();
    ensure_codex_cli_command_env_for_target(
        &mut provider_env,
        &cfg,
        "codex",
        Some(InstallTarget::Container),
    )
    .expect("inject codex env");

    assert_eq!(
        provider_env.get("CTX_CODEX_BIN_PATH"),
        Some(
            &std::fs::canonicalize(&container_codex)
                .expect("canonicalize container codex")
                .to_string_lossy()
                .to_string()
        )
    );
}

#[test]
fn codex_env_preserves_existing_explicit_codex_bin_path() {
    let tmp = tempdir().expect("tempdir");
    let codex_bin = tmp.path().join("codex");
    std::fs::write(&codex_bin, b"#!/bin/sh\n").expect("write codex");
    let expected = std::fs::canonicalize(&codex_bin)
        .expect("canonicalize codex")
        .to_string_lossy()
        .to_string();
    let mut provider_env = HashMap::from([(
        "CTX_CODEX_BIN_PATH".to_string(),
        codex_bin.to_string_lossy().to_string(),
    )]);
    ensure_codex_cli_command_env_for_target(
        &mut provider_env,
        &AgentServerConfigFile::default(),
        "codex",
        Some(InstallTarget::Host),
    )
    .expect("preserve existing codex path");
    assert_eq!(
        provider_env.get("CTX_CODEX_BIN_PATH").map(String::as_str),
        Some(expected.as_str())
    );
}

#[test]
fn codex_env_rejects_relative_explicit_codex_bin_path() {
    let mut provider_env = HashMap::from([("CTX_CODEX_BIN_PATH".to_string(), "codex".to_string())]);
    let err = ensure_codex_cli_command_env_for_target(
        &mut provider_env,
        &AgentServerConfigFile::default(),
        "codex",
        Some(InstallTarget::Host),
    )
    .expect_err("relative codex path should fail");
    assert!(
        err.to_string()
            .contains("CTX_CODEX_BIN_PATH must be an absolute path"),
        "unexpected error: {err:#}"
    );
}

#[tokio::test]
async fn codex_subscription_env_for_sandbox_uses_runtime_root_projection() {
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let data_dir = tempdir().expect("data dir");
    let runtime_dir = tempdir().expect("runtime dir");
    let root = data_dir.path();
    let runtime_root = runtime_dir.path();

    let host_home = codex_runtime_home(root);
    std::fs::create_dir_all(&host_home).expect("host codex home");
    std::fs::write(
        host_home.join("auth.json"),
        br#"{"OPENAI_API_KEY":"stale-host-key"}"#,
    )
    .expect("write stale host auth");

    let registry = CodexAccountRegistry {
        active_account_id: Some("acct-123".to_string()),
        accounts: vec![CodexAccountEntry {
            id: "acct-123".to_string(),
            label: "Account".to_string(),
            kind: CODEX_CREDENTIAL_KIND_API_KEY.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: None,
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry)
        .await
        .expect("save codex registry");
    let account_dir = ensure_codex_account_dir(root, "acct-123")
        .await
        .expect("account dir");
    tokio::fs::write(
        account_dir.join("auth.json"),
        br#"{"OPENAI_API_KEY":"fresh-active-key"}"#,
    )
    .await
    .expect("write account auth");

    let env = codex_env_for_active_account_with_runtime_root(root, runtime_root)
        .await
        .expect("sandbox codex env");
    let codex_home = env.get("CODEX_HOME").expect("CODEX_HOME");
    assert_eq!(
        Path::new(codex_home),
        codex_runtime_home(runtime_root).as_path(),
        "sandbox launch should use runtime-root Codex home, not host home",
    );

    let projected = tokio::fs::read_to_string(codex_runtime_home(runtime_root).join("auth.json"))
        .await
        .expect("read projected auth");
    assert!(
        projected.contains("fresh-active-key"),
        "sandbox projection should use active account auth: {projected}"
    );
    assert!(
        !projected.contains("stale-host-key"),
        "sandbox projection must not reuse stale host auth: {projected}"
    );
}
