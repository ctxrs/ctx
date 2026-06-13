use super::*;

#[test]
fn runtime_path_includes_command_parent_before_existing_path() {
    let tmp = tempdir().expect("tempdir");
    let data_root = tmp.path().join("data");
    let provider_bin_dir = tmp.path().join("provider-bin");
    std::fs::create_dir_all(&data_root).expect("data_root");
    std::fs::create_dir_all(&provider_bin_dir).expect("provider_bin_dir");
    let provider_cmd = provider_bin_dir.join("provider-cmd");
    std::fs::write(&provider_cmd, b"#!/bin/sh\n").expect("provider_cmd");

    let mut cfg = AgentServerConfigFile::default();
    cfg.providers.insert(
        "test-provider".to_string(),
        AgentServerCommand {
            command: provider_cmd.to_string_lossy().to_string(),
            args: Vec::new(),
            dependencies: Vec::new(),
            managed: None,
        },
    );

    let mut provider_env = HashMap::new();
    provider_env.insert("PATH".to_string(), "/usr/bin".to_string());
    installer::prepend_runtime_bin_dirs_to_provider_path_for_target(
        &mut provider_env,
        &cfg,
        "test-provider",
        &data_root,
        None,
    );

    let path_value = provider_env.get("PATH").expect("path");
    let split: Vec<PathBuf> = std::env::split_paths(std::ffi::OsStr::new(path_value)).collect();
    let expected_first =
        std::fs::canonicalize(&provider_bin_dir).expect("canonical provider_bin_dir");
    assert_eq!(split.first().expect("first path"), &expected_first);
}

#[test]
fn runtime_path_includes_dependency_bin_dirs() {
    let tmp = tempdir().expect("tempdir");
    let data_root = tmp.path().join("data");
    let provider_bin_dir = tmp.path().join("provider-bin");
    let managed_bin_rel = "managed/dep/bin";
    let managed_bin_dir = data_root.join(managed_bin_rel);
    std::fs::create_dir_all(&managed_bin_dir).expect("managed_bin_dir");
    std::fs::create_dir_all(&provider_bin_dir).expect("provider_bin_dir");
    let provider_cmd = provider_bin_dir.join("provider-cmd");
    std::fs::write(&provider_cmd, b"#!/bin/sh\n").expect("provider_cmd");

    let mut cfg = AgentServerConfigFile::default();
    cfg.providers.insert(
        "test-provider".to_string(),
        AgentServerCommand {
            command: provider_cmd.to_string_lossy().to_string(),
            args: Vec::new(),
            dependencies: vec!["dep-node".to_string()],
            managed: None,
        },
    );
    cfg.managed_installs.insert(
        "dep-node".to_string(),
        ManagedInstallMetadata {
            package: None,
            version: None,
            artifact_fingerprint: None,
            archive_sha256: None,
            target: None,
            install_dir_rel: None,
            bin_dir_rel: Some(managed_bin_rel.to_string()),
            last_success_at: None,
            last_error: None,
        },
    );

    let mut provider_env = HashMap::new();
    provider_env.insert("PATH".to_string(), "/usr/bin".to_string());
    installer::prepend_runtime_bin_dirs_to_provider_path_for_target(
        &mut provider_env,
        &cfg,
        "test-provider",
        &data_root,
        None,
    );

    let path_value = provider_env.get("PATH").expect("path");
    let split: Vec<PathBuf> = std::env::split_paths(std::ffi::OsStr::new(path_value)).collect();
    let expected_first =
        std::fs::canonicalize(&provider_bin_dir).expect("canonical provider_bin_dir");
    assert_eq!(split.first().expect("first path"), &expected_first);
    assert_eq!(split.get(1).expect("second path"), &managed_bin_dir);
}

#[test]
fn runtime_path_includes_target_specific_managed_provider_dependency_bin_dirs() {
    let tmp = tempdir().expect("tempdir");
    let data_root = tmp.path().join("data");
    let provider_bin_dir = tmp.path().join("provider-bin");
    let dependency_bin_dir = tmp.path().join("dependency-bin");
    std::fs::create_dir_all(&data_root).expect("data_root");
    std::fs::create_dir_all(&provider_bin_dir).expect("provider_bin_dir");
    std::fs::create_dir_all(&dependency_bin_dir).expect("dependency_bin_dir");
    let provider_cmd = provider_bin_dir.join("provider-cmd");
    let dependency_cmd = dependency_bin_dir.join("codex");
    std::fs::write(&provider_cmd, b"#!/bin/sh\n").expect("provider_cmd");
    std::fs::write(&dependency_cmd, b"#!/bin/sh\n").expect("dependency_cmd");

    let mut cfg = AgentServerConfigFile::default();
    cfg.managed_provider_targets.insert(
        "test-provider".to_string(),
        HashMap::from([(
            InstallTarget::Container.as_str().to_string(),
            AgentServerCommand {
                command: provider_cmd.to_string_lossy().to_string(),
                args: Vec::new(),
                dependencies: vec!["dep-provider".to_string()],
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
        )]),
    );
    cfg.managed_provider_targets.insert(
        "dep-provider".to_string(),
        HashMap::from([(
            InstallTarget::Container.as_str().to_string(),
            AgentServerCommand {
                command: dependency_cmd.to_string_lossy().to_string(),
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
        )]),
    );

    let mut provider_env = HashMap::new();
    provider_env.insert("PATH".to_string(), "/usr/bin".to_string());
    provider_env.insert(
        ctx_harness_runtime::CTX_HARNESS_LINUX_SANDBOX_ENV.to_string(),
        "1".to_string(),
    );
    installer::prepend_runtime_bin_dirs_to_provider_path_for_target(
        &mut provider_env,
        &cfg,
        "test-provider",
        &data_root,
        Some(InstallTarget::Container),
    );

    let path_value = provider_env.get("PATH").expect("path");
    let split: Vec<PathBuf> = std::env::split_paths(std::ffi::OsStr::new(path_value)).collect();
    let expected_first =
        std::fs::canonicalize(&provider_bin_dir).expect("canonical provider_bin_dir");
    let expected_second =
        std::fs::canonicalize(&dependency_bin_dir).expect("canonical dependency_bin_dir");
    assert_eq!(split.first().expect("first path"), &expected_first);
    assert_eq!(split.get(1).expect("second path"), &expected_second);
}
