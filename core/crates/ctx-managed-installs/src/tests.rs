use super::artifacts::{
    agent_server_download_tmp_name, commit_atomic_install_dir, prepare_atomic_install_dir,
    reject_download_redirect, resolve_download_resume, validate_expected_sha256,
    validate_sha256_digest, DownloadRedirectPolicy,
};
use super::toolchains::{
    node_runtime_target_for_install_target, python_target_can_use_bundled_runtime,
    resolve_python_bin,
};
use super::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn status_with_managed_target(target: &str) -> ctx_providers::adapters::ProviderStatus {
    let mut details = HashMap::new();
    details.insert("managed_target".to_string(), target.to_string());
    ctx_providers::adapters::ProviderStatus {
        provider_id: "codex".to_string(),
        installed: true,
        detected_path: Some("/tmp/codex".to_string()),
        version: None,
        capabilities: None,
        health: ctx_providers::adapters::ProviderHealth::Ok,
        diagnostics: Vec::new(),
        details,
        usability: ctx_providers::adapters::ProviderUsability::default(),
    }
}

fn host_detected_status() -> ctx_providers::adapters::ProviderStatus {
    ctx_providers::adapters::ProviderStatus {
        provider_id: "codex".to_string(),
        installed: true,
        detected_path: Some("/usr/local/bin/codex".to_string()),
        version: None,
        capabilities: None,
        health: ctx_providers::adapters::ProviderHealth::Ok,
        diagnostics: Vec::new(),
        details: HashMap::new(),
        usability: ctx_providers::adapters::ProviderUsability::default(),
    }
}

#[test]
fn managed_provider_installs_are_enabled_for_supported_entries() {
    let matrix = provider_matrix::builtin_matrix();
    assert!(is_supported_managed_provider(&matrix, "codex"));
    assert!(is_supported_managed_provider(&matrix, "opencode"));
}

#[test]
fn parse_install_target_defaults_to_host() {
    assert_eq!(
        parse_install_target(None).expect("default install target"),
        InstallTarget::Host
    );
}

#[test]
fn parse_install_target_rejects_unknown_values() {
    let err = parse_install_target(Some("not-a-target")).expect_err("invalid target should fail");
    assert!(err.to_string().contains("invalid install target"));
}

#[test]
fn archive_bin_requires_node_runtime_detects_javascript_entrypoints() {
    let missing = Path::new("/__ctx_missing_entrypoint__");
    assert!(archive_bin_requires_node_runtime(
        "dist/bin/amp-acp.js",
        missing
    ));
    assert!(archive_bin_requires_node_runtime(
        "dist/bin/provider.mjs",
        missing
    ));
    assert!(archive_bin_requires_node_runtime(
        "dist/bin/provider.cjs",
        missing
    ));
    assert!(!archive_bin_requires_node_runtime(
        "dist/bin/provider",
        missing
    ));
    assert!(!archive_bin_requires_node_runtime(
        "dist/bin/provider.exe",
        missing
    ));
}

#[test]
fn archive_bin_requires_node_runtime_detects_extensionless_node_shebang() {
    let temp = tempfile::tempdir().expect("tempdir");
    let launcher = temp.path().join("claude-crp");
    std::fs::write(&launcher, "#!/usr/bin/env node\nconsole.log('ctx');\n")
        .expect("write launcher");
    assert!(archive_bin_requires_node_runtime(
        "bin/claude-crp",
        &launcher
    ));
}

#[test]
fn archive_bin_requires_node_runtime_detects_env_shebang_with_flags() {
    let temp = tempfile::tempdir().expect("tempdir");
    let launcher = temp.path().join("provider");
    std::fs::write(
        &launcher,
        "#!/usr/bin/env -S node --no-warnings\nconsole.log('ctx');\n",
    )
    .expect("write launcher");
    assert!(archive_bin_requires_node_runtime("bin/provider", &launcher));
}

#[test]
fn archive_bin_requires_node_runtime_ignores_non_node_shebang() {
    let temp = tempfile::tempdir().expect("tempdir");
    let launcher = temp.path().join("provider");
    std::fs::write(&launcher, "#!/bin/sh\necho ctx\n").expect("write launcher");
    assert!(!archive_bin_requires_node_runtime(
        "bin/provider",
        &launcher
    ));
}

#[test]
fn provider_archive_sha256_validation_requires_full_hex_digest() {
    validate_expected_sha256("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        .expect("valid sha256");
    assert!(validate_expected_sha256("").is_err());
    assert!(validate_expected_sha256("abc123").is_err());
    assert!(validate_expected_sha256(
        "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
    )
    .is_err());
}

#[test]
fn managed_runtime_lock_uses_ctx_mirror_only() {
    for spec in runtime_lock::all_runtime_archive_specs() {
        runtime_lock::validate_runtime_archive_spec(spec).expect("valid runtime lock entry");
        assert!(
            spec.mirror_url.starts_with(
                "https://api.ctx.rs/storage/v1/object/public/releases/artifacts/managed-runtimes/"
            ),
            "runtime URL must use ctx storage mirror: {}",
            spec.mirror_url
        );
        assert!(
            !spec.mirror_url.contains("nodejs.org")
                && !spec.mirror_url.contains("github.com")
                && !spec.mirror_url.contains("python-build-standalone")
                && !spec.mirror_url.contains("supabase.co"),
            "runtime URL must not point at upstream: {}",
            spec.mirror_url
        );
    }
}

#[test]
fn managed_runtime_lock_covers_supported_node_targets() {
    let targets = [
        (
            "darwin-arm64",
            runtime_lock::ManagedRuntimeArchiveKind::TarGz,
        ),
        ("darwin-x64", runtime_lock::ManagedRuntimeArchiveKind::TarGz),
        (
            "linux-arm64",
            runtime_lock::ManagedRuntimeArchiveKind::TarGz,
        ),
        ("linux-x64", runtime_lock::ManagedRuntimeArchiveKind::TarGz),
        ("win-arm64", runtime_lock::ManagedRuntimeArchiveKind::Zip),
        ("win-x64", runtime_lock::ManagedRuntimeArchiveKind::Zip),
    ];
    for (target, archive_kind) in targets {
        let spec = runtime_lock::resolve_node_runtime_archive(NODE_VERSION, target, archive_kind)
            .expect("node target must be locked");
        assert_eq!(spec.kind, runtime_lock::ManagedRuntimeKind::Node);
        assert_eq!(spec.version, NODE_VERSION);
        assert_eq!(spec.target, target);
        assert!(spec
            .content_scoped_install_dir_name()
            .contains(&format!("sha256-{}", spec.sha256_prefix())));
    }
}

#[test]
fn managed_runtime_lock_covers_python_defaults_and_provider_overrides() {
    let mut tuples = HashSet::from([(PYTHON_VERSION.to_string(), PYTHON_BUILD_TAG.to_string())]);
    let matrix = provider_matrix::builtin_matrix();
    for entry in matrix.providers {
        let Some(provider_matrix::ProviderInstall::Python {
            python_version,
            python_build_tag,
            ..
        }) = entry.managed_install
        else {
            continue;
        };
        let runtime =
            managed_python_runtime_spec(python_version.as_deref(), python_build_tag.as_deref());
        tuples.insert((runtime.version, runtime.build_tag));
    }

    assert!(
        tuples.contains(&("3.12.13".to_string(), "20260303".to_string())),
        "provider-matrix Python override must stay covered"
    );

    let targets = [
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "aarch64-unknown-linux-gnu",
        "x86_64-unknown-linux-gnu",
        "aarch64-pc-windows-msvc",
        "x86_64-pc-windows-msvc",
    ];
    for (version, build_tag) in tuples {
        for target in targets {
            let spec = runtime_lock::resolve_python_runtime_archive(&version, &build_tag, target)
                .expect("python target must be locked");
            assert_eq!(spec.kind, runtime_lock::ManagedRuntimeKind::Python);
            assert_eq!(spec.version, version);
            assert_eq!(spec.build_tag, Some(build_tag.as_str()));
            assert_eq!(spec.target, target);
        }
    }
}

#[test]
fn managed_runtime_lock_rejects_upstream_hosts_and_invalid_sha() {
    let valid = *runtime_lock::resolve_node_runtime_archive(
        NODE_VERSION,
        "linux-x64",
        runtime_lock::ManagedRuntimeArchiveKind::TarGz,
    )
    .expect("node lock entry");

    let mut upstream = valid;
    upstream.mirror_url = "https://nodejs.org/dist/v24.15.0/node-v24.15.0-linux-x64.tar.gz";
    let upstream_error = runtime_lock::validate_runtime_archive_spec(&upstream)
        .expect_err("upstream host must fail");
    assert!(upstream_error.to_string().contains("api.ctx.rs"));

    let mut invalid_sha = valid;
    invalid_sha.sha256 = "abc123";
    assert!(runtime_lock::validate_runtime_archive_spec(&invalid_sha).is_err());
}

#[tokio::test]
async fn managed_runtime_ready_metadata_requires_matching_lock_entry() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("node-runtime");
    tokio::fs::create_dir_all(&root)
        .await
        .expect("mkdir runtime");
    let spec = *runtime_lock::resolve_node_runtime_archive(
        NODE_VERSION,
        "linux-x64",
        runtime_lock::ManagedRuntimeArchiveKind::TarGz,
    )
    .expect("node lock entry");

    assert!(!runtime_lock::runtime_ready_metadata_matches(&root, &spec).await);
    runtime_lock::write_runtime_ready_metadata(&root, &spec)
        .await
        .expect("write ready metadata");
    assert!(runtime_lock::runtime_ready_metadata_matches(&root, &spec).await);

    let mut changed = spec;
    changed.sha256 = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
    assert!(
        !runtime_lock::runtime_ready_metadata_matches(&root, &changed).await,
        "same root must not be ready for a changed runtime hash"
    );
}

#[test]
fn managed_runtime_download_rejects_redirect_responses() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::LOCATION,
        reqwest::header::HeaderValue::from_static(
            "https://nodejs.org/dist/v24.15.0/node-v24.15.0-linux-x64.tar.gz",
        ),
    );

    let error = reject_download_redirect(
        DownloadRedirectPolicy::ManagedRuntimeMirrorOnly,
        reqwest::StatusCode::FOUND,
        &headers,
    )
    .expect_err("managed runtime redirect must fail");
    assert!(error.to_string().contains("redirected"));

    reject_download_redirect(
        DownloadRedirectPolicy::Follow,
        reqwest::StatusCode::FOUND,
        &headers,
    )
    .expect("default downloader may follow redirects");
    reject_download_redirect(
        DownloadRedirectPolicy::ManagedRuntimeMirrorOnly,
        reqwest::StatusCode::OK,
        &headers,
    )
    .expect("non-redirect response is allowed");
}

#[test]
fn managed_runtime_download_allows_only_ctx_mirror_storage_urls() {
    let storage_url = url::Url::parse(
        "https://api.ctx.rs/storage/v1/object/public/releases/artifacts/managed-runtimes/node/24.15.0/node-v24.15.0-linux-x64.tar.gz",
    )
    .expect("storage url");
    let function_url = url::Url::parse(
        "https://api.ctx.rs/functions/v1/download/managed-runtimes/node/24.15.0/node-v24.15.0-linux-x64.tar.gz",
    )
    .expect("function url");
    let raw_supabase_storage_url = url::Url::parse(
        "https://zaekxxfswfrotkxfvhpk.supabase.co/storage/v1/object/public/releases/artifacts/managed-runtimes/node/24.15.0/node-v24.15.0-linux-x64.tar.gz",
    )
    .expect("raw supabase storage url");
    let upstream_url =
        url::Url::parse("https://nodejs.org/dist/v24.15.0/node-v24.15.0-linux-x64.tar.gz")
            .expect("upstream url");

    assert!(runtime_lock::runtime_download_url_allowed(&storage_url));
    assert!(!runtime_lock::runtime_download_url_allowed(&function_url));
    assert!(!runtime_lock::runtime_download_url_allowed(
        &raw_supabase_storage_url
    ));
    assert!(!runtime_lock::runtime_download_url_allowed(&upstream_url));
}

#[test]
fn node_runtime_dependency_id_is_target_specific() {
    assert_eq!(
        node_runtime_dependency_id(InstallTarget::Host),
        "runtime-node-host"
    );
    assert_eq!(
        node_runtime_dependency_id(InstallTarget::Container),
        "runtime-node-container"
    );
    assert_eq!(
        node_runtime_dependency_id(InstallTarget::LinuxAarch64),
        "runtime-node-linux-aarch64"
    );
}

#[test]
fn node_runtime_dependency_metadata_records_runtime_sha() {
    let data_root = tempfile::tempdir().expect("tempdir");
    let sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let node_root = data_root.path().join("runtimes/node/node-v24.15.0-test");
    let node = NodeRuntime {
        node_root: node_root.clone(),
        node_bin: node_root.join("bin/node"),
        npm_cli_js: node_root.join("lib/node_modules/npm/bin/npm-cli.js"),
        archive_sha256: Some(sha256.to_string()),
    };

    let metadata = node_runtime_dependency_metadata(data_root.path(), &node, InstallTarget::Host);

    assert_eq!(metadata.archive_sha256.as_deref(), Some(sha256));
    let expected_fingerprint = format!("runtime:node:{NODE_VERSION}:sha256:{sha256}");
    assert_eq!(
        metadata.artifact_fingerprint.as_deref(),
        Some(expected_fingerprint.as_str())
    );
}

#[test]
fn container_node_runtime_target_is_linux_for_host_arch() {
    let target = node_runtime_target_for_install_target(InstallTarget::Container)
        .expect("container target mapping");
    match std::env::consts::ARCH {
        "aarch64" => assert_eq!(target.dist_target, "linux-arm64"),
        "x86_64" => assert_eq!(target.dist_target, "linux-x64"),
        other => panic!("unexpected test arch: {other}"),
    }
    assert!(!target.is_windows);
}

#[test]
fn container_node_runtime_dependency_targets_include_host_on_non_linux() {
    assert_eq!(
        node_runtime_dependency_targets_for_install_target(InstallTarget::Container, "macos"),
        vec![InstallTarget::Container, InstallTarget::Host]
    );
    assert_eq!(
        node_runtime_dependency_targets_for_install_target(InstallTarget::Container, "windows"),
        vec![InstallTarget::Container, InstallTarget::Host]
    );
    assert_eq!(
        node_runtime_dependency_targets_for_install_target(InstallTarget::Container, "linux"),
        vec![InstallTarget::Container]
    );
}

#[test]
fn managed_provider_runtime_command_wraps_acp_providers_with_bridge() {
    let data_root = tempfile::tempdir().expect("tempdir");
    let managed = AgentServerCommand {
        command: "/tmp/opencode".to_string(),
        args: vec!["acp".to_string()],
        dependencies: Vec::new(),
        managed: None,
    };
    let bridge = AgentServerCommand {
        command: "/tmp/acp-crp-bridge".to_string(),
        args: vec!["--stdio".to_string()],
        dependencies: Vec::new(),
        managed: None,
    };

    let runtime =
        managed_provider_runtime_command(data_root.path(), "opencode", managed, Some(&bridge))
            .expect("wrapped runtime command");

    assert_eq!(runtime.command, "/tmp/acp-crp-bridge");
    assert_eq!(runtime.args.first().map(String::as_str), Some("--stdio"));
    assert!(
        runtime.args.iter().any(|arg| arg == "--acp-command"),
        "bridge command must include ACP command wrapper"
    );
    assert!(
        runtime.args.iter().any(|arg| arg == "/tmp/opencode acp"),
        "bridge command should point at the installed ACP command"
    );
}

#[test]
fn managed_provider_runtime_command_rejects_path_style_gemini_runtime() {
    let data_root = tempfile::tempdir().expect("tempdir");
    let gemini_bin = data_root.path().join("bundle").join("bin").join("gemini");
    std::fs::create_dir_all(gemini_bin.parent().expect("parent")).expect("mkdir gemini");
    std::fs::write(&gemini_bin, b"gemini").expect("write gemini");
    let managed = AgentServerCommand {
        command: gemini_bin.to_string_lossy().to_string(),
        args: vec!["--experimental-acp".to_string()],
        dependencies: Vec::new(),
        managed: None,
    };
    let bridge = AgentServerCommand {
        command: "/tmp/acp-crp-bridge".to_string(),
        args: vec!["--stdio".to_string()],
        dependencies: Vec::new(),
        managed: None,
    };

    let err = managed_provider_runtime_command(data_root.path(), "gemini", managed, Some(&bridge))
        .unwrap_err();

    assert!(err
        .to_string()
        .contains("must use an explicit absolute node executable"));
}

fn create_managed_gemini_runtime_layout(root: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
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
    let package_json = cli_entry
        .parent()
        .and_then(|parent| parent.parent())
        .expect("gemini cli root")
        .join("package.json");
    let core_entry = cli_entry
        .parent()
        .expect("bundle dir")
        .join("core-ctx-test.js");
    std::fs::create_dir_all(node_bin.parent().expect("node parent")).expect("mkdir node");
    std::fs::create_dir_all(cli_entry.parent().expect("cli parent")).expect("mkdir gemini");
    std::fs::write(&node_bin, b"node").expect("write node");
    std::fs::write(&cli_entry, b"gemini").expect("write cli");
    std::fs::write(
        &core_entry,
        "export const coreEvents = {}; export const CoreEvent = {}; export const writeToStdout = () => {}; export const writeToStderr = () => {};",
    )
    .expect("write core");
    std::fs::write(
        &package_json,
        r#"{"name":"@google/gemini-cli","version":"0.38.2"}"#,
    )
    .expect("write package");
    (node_bin, cli_entry, package_json, core_entry)
}

fn gemini_managed_command(node_bin: &Path, cli_entry: &Path) -> AgentServerCommand {
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

fn acp_bridge_command() -> AgentServerCommand {
    AgentServerCommand {
        command: "/tmp/acp-crp-bridge".to_string(),
        args: vec!["--stdio".to_string()],
        dependencies: Vec::new(),
        managed: None,
    }
}

#[test]
fn managed_provider_runtime_command_wraps_gemini_runtime_with_wrapper() {
    let data_root = tempfile::tempdir().expect("tempdir");
    let node_bin = data_root
        .path()
        .join("bundle")
        .join("runtimes")
        .join("node")
        .join("bin")
        .join("node");
    let cli_entry = data_root
        .path()
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
    let package_json = cli_entry
        .parent()
        .and_then(|parent| parent.parent())
        .expect("gemini cli root")
        .join("package.json");
    std::fs::create_dir_all(node_bin.parent().expect("node parent")).expect("mkdir node");
    std::fs::create_dir_all(cli_entry.parent().expect("cli parent")).expect("mkdir gemini");
    std::fs::write(&node_bin, b"node").expect("write node");
    std::fs::write(&cli_entry, b"gemini").expect("write cli");
    std::fs::write(
        &core_entry,
        "export const coreEvents = {}; export const CoreEvent = {}; export const writeToStdout = () => {}; export const writeToStderr = () => {};",
    )
    .expect("write core");
    std::fs::write(
        &package_json,
        r#"{"name":"@google/gemini-cli","version":"0.38.2"}"#,
    )
    .expect("write package");
    let managed = AgentServerCommand {
        command: node_bin.to_string_lossy().to_string(),
        args: vec![
            cli_entry.to_string_lossy().to_string(),
            "--experimental-acp".to_string(),
        ],
        dependencies: Vec::new(),
        managed: None,
    };
    let bridge = AgentServerCommand {
        command: "/tmp/acp-crp-bridge".to_string(),
        args: vec!["--stdio".to_string()],
        dependencies: Vec::new(),
        managed: None,
    };

    let runtime =
        managed_provider_runtime_command(data_root.path(), "gemini", managed, Some(&bridge))
            .expect("wrapped runtime");

    let acp_command_index = runtime
        .args
        .iter()
        .position(|arg| arg == "--acp-command")
        .expect("missing --acp-command");
    let acp_command = runtime
        .args
        .get(acp_command_index + 1)
        .expect("missing wrapped acp command");
    let wrapper_path = data_root
        .path()
        .join("providers")
        .join("agent-servers")
        .join("gemini-acp-wrapper.mjs");

    assert_eq!(runtime.command, "/tmp/acp-crp-bridge");
    assert!(
        acp_command.contains(&node_bin.to_string_lossy().to_string()),
        "bridge should launch Gemini through the explicit node runtime"
    );
    assert!(
        acp_command.contains(&wrapper_path.to_string_lossy().to_string()),
        "bridge should launch the generated Gemini ACP wrapper"
    );
    let wrapper = std::fs::read_to_string(&wrapper_path).expect("read Gemini wrapper");
    assert!(wrapper.contains(core_entry.to_string_lossy().as_ref()));
    assert!(wrapper.contains(cli_entry.to_string_lossy().as_ref()));
    assert!(wrapper.contains("CoreEvent.ConsentRequest"));
    assert!(wrapper.contains("GEMINI_CLI_NO_RELAUNCH"));
}

#[test]
fn managed_provider_runtime_command_rejects_gemini_runtime_with_missing_package_json() {
    let data_root = tempfile::tempdir().expect("tempdir");
    let (node_bin, cli_entry, package_json, _) =
        create_managed_gemini_runtime_layout(data_root.path());
    std::fs::remove_file(package_json).expect("remove package json");

    let err = managed_provider_runtime_command(
        data_root.path(),
        "gemini",
        gemini_managed_command(&node_bin, &cli_entry),
        Some(&acp_bridge_command()),
    )
    .expect_err("missing package json should fail");

    assert!(err.to_string().contains(
        "Gemini ACP entrypoint must live under a node_modules/@google/gemini-cli install tree"
    ));
}

#[test]
fn managed_provider_runtime_command_rejects_gemini_runtime_with_missing_core_entry() {
    let data_root = tempfile::tempdir().expect("tempdir");
    let (node_bin, cli_entry, _, core_entry) =
        create_managed_gemini_runtime_layout(data_root.path());
    std::fs::remove_file(core_entry).expect("remove core entry");

    let err = managed_provider_runtime_command(
        data_root.path(),
        "gemini",
        gemini_managed_command(&node_bin, &cli_entry),
        Some(&acp_bridge_command()),
    )
    .expect_err("missing core entry should fail");

    assert!(err
        .to_string()
        .contains("Gemini ACP bundled core entrypoint is missing"));
}

#[test]
fn managed_provider_runtime_command_accepts_gemini_runtime_with_multiple_core_entries() {
    let data_root = tempfile::tempdir().expect("tempdir");
    let node_bin = data_root
        .path()
        .join("bundle")
        .join("runtimes")
        .join("node")
        .join("bin")
        .join("node");
    let cli_entry = data_root
        .path()
        .join("bundle")
        .join("providers")
        .join("gemini")
        .join("node_modules")
        .join("@google")
        .join("gemini-cli")
        .join("bundle")
        .join("gemini.js");
    let package_json = cli_entry
        .parent()
        .and_then(|parent| parent.parent())
        .expect("gemini cli root")
        .join("package.json");
    std::fs::create_dir_all(node_bin.parent().expect("node parent")).expect("mkdir node");
    std::fs::create_dir_all(cli_entry.parent().expect("cli parent")).expect("mkdir gemini");
    std::fs::write(&node_bin, b"node").expect("write node");
    std::fs::write(&cli_entry, b"gemini").expect("write cli");
    std::fs::write(
        cli_entry.parent().expect("bundle dir").join("core-alpha.js"),
        "export const coreEvents = {}; export const CoreEvent = {}; export const writeToStdout = () => {}; export const writeToStderr = () => {};",
    )
    .expect("write core alpha");
    std::fs::write(
        cli_entry.parent().expect("bundle dir").join("core-beta.js"),
        "export const coreEvents = {}; export const CoreEvent = {}; export const writeToStdout = () => {}; export const writeToStderr = () => {};",
    )
    .expect("write core beta");
    std::fs::write(
        &package_json,
        r#"{"name":"@google/gemini-cli","version":"0.38.2"}"#,
    )
    .expect("write package");

    let runtime = managed_provider_runtime_command(
        data_root.path(),
        "gemini",
        AgentServerCommand {
            command: node_bin.to_string_lossy().to_string(),
            args: vec![
                cli_entry.to_string_lossy().to_string(),
                "--experimental-acp".to_string(),
            ],
            dependencies: Vec::new(),
            managed: None,
        },
        Some(&AgentServerCommand {
            command: "/tmp/acp-crp-bridge".to_string(),
            args: vec!["--stdio".to_string()],
            dependencies: Vec::new(),
            managed: None,
        }),
    )
    .expect("multiple core entries should be wrapped");

    let acp_command = runtime
        .args
        .windows(2)
        .find_map(|window| (window[0] == "--acp-command").then_some(window[1].as_str()))
        .expect("missing --acp-command");
    assert!(acp_command.contains("gemini-acp-wrapper.mjs"));
    let wrapper_path = data_root
        .path()
        .join("providers")
        .join("agent-servers")
        .join("gemini-acp-wrapper.mjs");
    let wrapper = std::fs::read_to_string(wrapper_path).expect("read Gemini wrapper");
    assert!(wrapper.contains("core-alpha.js"));
    assert!(wrapper.contains("core-beta.js"));
    assert!(wrapper.contains("coreCandidates.find"));
}

#[test]
fn managed_provider_runtime_command_wraps_goose_binary_with_acp_and_developer_builtin() {
    let data_root = tempfile::tempdir().expect("tempdir");
    let goose_bin = data_root.path().join("bin").join("goose");
    std::fs::create_dir_all(goose_bin.parent().expect("parent")).expect("mkdir goose");
    std::fs::write(&goose_bin, b"#!/bin/sh\nexit 0\n").expect("write goose");
    let managed = AgentServerCommand {
        command: goose_bin.to_string_lossy().to_string(),
        args: Vec::new(),
        dependencies: Vec::new(),
        managed: None,
    };
    let bridge = AgentServerCommand {
        command: "/tmp/acp-crp-bridge".to_string(),
        args: vec!["--stdio".to_string()],
        dependencies: Vec::new(),
        managed: None,
    };

    let runtime =
        managed_provider_runtime_command(data_root.path(), "goose", managed, Some(&bridge))
            .expect("wrapped runtime command");

    let acp_command_index = runtime
        .args
        .iter()
        .position(|arg| arg == "--acp-command")
        .expect("missing --acp-command");
    assert_eq!(runtime.command, "/tmp/acp-crp-bridge");
    assert_eq!(
        runtime.args.get(acp_command_index + 1),
        Some(&format!(
            "{} acp --with-builtin developer",
            goose_bin.to_string_lossy()
        ))
    );
}

#[test]
fn managed_provider_runtime_command_keeps_goose_shim_entrypoint_while_adding_developer_builtin() {
    let data_root = tempfile::tempdir().expect("tempdir");
    let goose_shim = data_root
        .path()
        .join("dist")
        .join("bin")
        .join("goose-acp.js");
    std::fs::create_dir_all(goose_shim.parent().expect("parent")).expect("mkdir shim");
    std::fs::write(&goose_shim, b"#!/usr/bin/env node\n").expect("write shim");
    let managed = AgentServerCommand {
        command: goose_shim.to_string_lossy().to_string(),
        args: Vec::new(),
        dependencies: Vec::new(),
        managed: None,
    };
    let bridge = AgentServerCommand {
        command: "/tmp/acp-crp-bridge".to_string(),
        args: vec!["--stdio".to_string()],
        dependencies: Vec::new(),
        managed: None,
    };

    let runtime =
        managed_provider_runtime_command(data_root.path(), "goose", managed, Some(&bridge))
            .expect("wrapped runtime command");

    let acp_command_index = runtime
        .args
        .iter()
        .position(|arg| arg == "--acp-command")
        .expect("missing --acp-command");
    assert_eq!(runtime.command, "/tmp/acp-crp-bridge");
    assert_eq!(
        runtime.args.get(acp_command_index + 1),
        Some(&format!(
            "{} --with-builtin developer",
            goose_shim.to_string_lossy()
        ))
    );
}

#[test]
fn managed_provider_runtime_command_keeps_native_crp_providers_raw() {
    let managed = AgentServerCommand {
        command: "/tmp/codex-crp".to_string(),
        args: vec!["--stdio".to_string()],
        dependencies: Vec::new(),
        managed: None,
    };

    let runtime = managed_provider_runtime_command(Path::new("/tmp"), "codex", managed, None)
        .expect("raw runtime command");

    assert_eq!(runtime.command, "/tmp/codex-crp");
    assert_eq!(runtime.args, vec!["--stdio".to_string()]);
}

#[test]
fn bundled_seed_js_runtime_prepends_bundled_node_bin_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let script = temp.path().join("providers/pi/macos/aarch64/pi-acp.js");
    std::fs::create_dir_all(script.parent().expect("parent")).expect("mkdir script");
    std::fs::write(&script, b"#!/usr/bin/env node\n").expect("write script");

    let node_bin = temp
        .path()
        .join("runtimes/node/macos/aarch64/node-v1/bin/node");
    std::fs::create_dir_all(node_bin.parent().expect("parent")).expect("mkdir node");
    std::fs::write(&node_bin, b"ok").expect("write node");

    let runtime_cmd = ProviderRuntimeCommand {
        provider_id: "pi".to_string(),
        command_abs_path: script.to_string_lossy().to_string(),
        args: Vec::new(),
        dependencies: Vec::new(),
        source: ProviderRuntimeCommandSource::BundledSeed,
    };
    let bundled_node = bundled_assets::BundledRuntimePaths {
        root: node_bin
            .parent()
            .expect("bin dir")
            .parent()
            .expect("runtime root")
            .to_path_buf(),
        bin: node_bin.clone(),
        npm_cli: None,
        version: "1".to_string(),
        sha256: "0".repeat(64),
    };

    let mut bin_dirs = vec![script.parent().expect("script dir").to_path_buf()];
    prepend_bundled_seed_node_bin_dir(&mut bin_dirs, &runtime_cmd, Some(bundled_node));

    assert!(bin_dirs.contains(&node_bin.parent().expect("node dir").to_path_buf()));
}

#[test]
fn dependency_target_compatibility_filters_linux_bins_for_non_linux_host_probes() {
    assert!(dependency_target_compatible_with_context(
        Some(InstallTarget::Host),
        false,
        "macos",
        "aarch64"
    ));
    assert!(!dependency_target_compatible_with_context(
        Some(InstallTarget::Container),
        false,
        "macos",
        "aarch64"
    ));
    assert!(!dependency_target_compatible_with_context(
        Some(InstallTarget::LinuxAarch64),
        false,
        "macos",
        "aarch64"
    ));
    assert!(dependency_target_compatible_with_context(
        Some(InstallTarget::Container),
        false,
        "linux",
        "x86_64"
    ));
}

#[test]
fn dependency_target_compatibility_allows_linux_bins_for_container_exec() {
    assert!(dependency_target_compatible_with_context(
        Some(InstallTarget::Container),
        true,
        "macos",
        "aarch64"
    ));
    assert!(dependency_target_compatible_with_context(
        Some(InstallTarget::LinuxAarch64),
        true,
        "windows",
        "aarch64"
    ));
    assert!(!dependency_target_compatible_with_context(
        Some(InstallTarget::Host),
        true,
        "linux",
        "x86_64"
    ));
}

#[test]
fn resolve_codex_cli_command_path_uses_requested_target() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let host_bin = tmp.path().join("codex-host");
    let container_bin = tmp.path().join("codex-container");
    std::fs::write(&host_bin, b"#!/bin/sh\n").expect("write host bin");
    std::fs::write(&container_bin, b"#!/bin/sh\n").expect("write container bin");

    let mut cfg = AgentServerConfigFile::default();
    cfg.managed_provider_targets.insert(
        "codex-cli".to_string(),
        HashMap::from([
            (
                InstallTarget::Host.as_str().to_string(),
                AgentServerCommand {
                    command: host_bin.to_string_lossy().to_string(),
                    args: Vec::new(),
                    dependencies: Vec::new(),
                    managed: None,
                },
            ),
            (
                InstallTarget::Container.as_str().to_string(),
                AgentServerCommand {
                    command: container_bin.to_string_lossy().to_string(),
                    args: Vec::new(),
                    dependencies: Vec::new(),
                    managed: None,
                },
            ),
        ]),
    );

    let host = resolve_codex_cli_command_path_for_target(&cfg, Some(InstallTarget::Host))
        .expect("resolve host codex-cli")
        .expect("host codex-cli");
    let container = resolve_codex_cli_command_path_for_target(&cfg, Some(InstallTarget::Container))
        .expect("resolve container codex-cli")
        .expect("container codex-cli");

    assert_eq!(
        host,
        std::fs::canonicalize(&host_bin)
            .expect("canonicalize host codex-cli")
            .to_string_lossy()
    );
    assert_eq!(
        container,
        std::fs::canonicalize(&container_bin)
            .expect("canonicalize container codex-cli")
            .to_string_lossy()
    );
}

#[test]
fn inject_codex_cli_command_env_sets_explicit_runtime_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let codex_bin = tmp.path().join("codex-aarch64-apple-darwin");
    std::fs::write(&codex_bin, b"#!/bin/sh\n").expect("write codex bin");

    let mut cfg = AgentServerConfigFile::default();
    cfg.managed_provider_targets.insert(
        "codex-cli".to_string(),
        HashMap::from([(
            InstallTarget::Host.as_str().to_string(),
            AgentServerCommand {
                command: codex_bin.to_string_lossy().to_string(),
                args: Vec::new(),
                dependencies: Vec::new(),
                managed: None,
            },
        )]),
    );

    let mut env = HashMap::new();
    ensure_codex_cli_command_env_for_target(&mut env, &cfg, "codex", Some(InstallTarget::Host))
        .expect("inject codex env");

    assert_eq!(
        env.get("CTX_CODEX_BIN_PATH"),
        Some(
            &std::fs::canonicalize(&codex_bin)
                .expect("canonicalize codex bin")
                .to_string_lossy()
                .to_string()
        )
    );
}

#[test]
fn ensure_codex_cli_command_env_rejects_missing_runtime_path() {
    let err = ensure_codex_cli_command_env_for_target(
        &mut HashMap::new(),
        &AgentServerConfigFile::default(),
        "codex",
        Some(InstallTarget::Host),
    )
    .expect_err("missing codex-cli path should fail");

    assert!(
        err.to_string()
            .contains("explicit codex-cli runtime path is not configured"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn provider_env_linux_sandbox_marker_enables_container_targeting() {
    let mut env = HashMap::new();
    assert!(!provider_env_targets_linux_sandbox(&env));

    env.insert(
        ctx_harness_runtime::CTX_HARNESS_LINUX_SANDBOX_ENV.to_string(),
        "1".to_string(),
    );
    assert!(provider_env_targets_linux_sandbox(&env));
}

#[test]
fn managed_provider_target_support_matches_install_kind() {
    let matrix = provider_matrix::builtin_matrix();
    let harness_provider_ids = [
        "claude-crp",
        "codex",
        "qwen",
        "cursor",
        "pi",
        "amp",
        "droid",
        "gemini",
        "copilot",
        "opencode",
        "cline",
        "mistral",
        "auggie",
        "goose",
        "kimi",
        "openhands",
    ];
    let mut archive_count = 0usize;
    let mut expected_container_supported = 0usize;
    assert_eq!(
        harness_provider_ids.len(),
        16,
        "curated harness list changed; update coverage expectation"
    );
    let host_supported = harness_provider_ids
        .iter()
        .filter(|provider_id| {
            is_supported_managed_provider_for_target(&matrix, provider_id, InstallTarget::Host)
        })
        .count();
    assert_eq!(
        host_supported,
        harness_provider_ids.len(),
        "expected full harness support for host: {host_supported}/{}",
        harness_provider_ids.len()
    );
    for provider_id in harness_provider_ids {
        let entry = provider_matrix::get_entry(&matrix, provider_id)
            .unwrap_or_else(|| panic!("missing provider matrix entry for {provider_id}"));
        let install = entry
            .managed_install
            .as_ref()
            .unwrap_or_else(|| panic!("missing managed_install for {provider_id}"));
        match install {
            provider_matrix::ProviderInstall::Archive { .. } => {
                archive_count += 1;
                expected_container_supported += 1;
                assert!(
                    is_supported_managed_provider_for_target(
                        &matrix,
                        provider_id,
                        InstallTarget::LinuxAarch64
                    ),
                    "archive provider {provider_id} missing linux-aarch64 support"
                );
                assert!(
                    is_supported_managed_provider_for_target(
                        &matrix,
                        provider_id,
                        InstallTarget::LinuxX8664
                    ),
                    "archive provider {provider_id} missing linux-x86_64 support"
                );
            }
            provider_matrix::ProviderInstall::Npm { .. }
            | provider_matrix::ProviderInstall::Python { .. } => {
                assert!(
                    is_supported_managed_provider_for_target(
                        &matrix,
                        provider_id,
                        InstallTarget::Host
                    ),
                    "managed provider {provider_id} must support host installs"
                );
                if install.archive_target("container").is_some() {
                    expected_container_supported += 1;
                    assert!(
                        is_supported_managed_provider_for_target(
                            &matrix,
                            provider_id,
                            InstallTarget::Container
                        ),
                        "hybrid provider {provider_id} missing container support"
                    );
                } else {
                    assert!(
                        !is_supported_managed_provider_for_target(
                            &matrix,
                            provider_id,
                            InstallTarget::Container
                        ),
                        "provider {provider_id} unexpectedly supports container without a staged target"
                    );
                }
            }
        }
    }
    let actual_container_supported = harness_provider_ids
        .iter()
        .filter(|provider_id| {
            is_supported_managed_provider_for_target(&matrix, provider_id, InstallTarget::Container)
        })
        .count();
    assert_eq!(
        actual_container_supported, expected_container_supported,
        "container support should track archive providers plus hybrid providers with staged container targets"
    );
    assert_eq!(
        archive_count, 7,
        "curated harness archive set changed; verify linux target coverage expectations"
    );
}

#[test]
fn managed_python_runtime_spec_defaults_to_global_python_runtime() {
    let spec = managed_python_runtime_spec(None, None);

    assert_eq!(
        spec,
        ManagedPythonRuntimeSpec {
            version: PYTHON_VERSION.to_string(),
            build_tag: PYTHON_BUILD_TAG.to_string(),
        }
    );
}

#[test]
fn managed_python_runtime_spec_allows_provider_scoped_override() {
    let spec = managed_python_runtime_spec(Some("3.12.13"), Some("20260303"));

    assert_eq!(
        spec,
        ManagedPythonRuntimeSpec {
            version: "3.12.13".to_string(),
            build_tag: "20260303".to_string(),
        }
    );
}

#[test]
fn python_bundled_runtime_is_host_only() {
    assert!(python_target_can_use_bundled_runtime(InstallTarget::Host));
    assert!(!python_target_can_use_bundled_runtime(
        InstallTarget::Container
    ));
    assert!(!python_target_can_use_bundled_runtime(
        InstallTarget::LinuxAarch64
    ));
    assert!(!python_target_can_use_bundled_runtime(
        InstallTarget::LinuxX8664
    ));
}

#[test]
fn expected_managed_dependency_version_detects_runtime_dependencies() {
    assert_eq!(
        expected_managed_dependency_version("runtime-node-host"),
        Some(NODE_VERSION)
    );
    assert_eq!(
        expected_managed_dependency_version("runtime-python-container"),
        Some(PYTHON_VERSION)
    );
    assert_eq!(expected_managed_dependency_version("codex"), None);
}

#[test]
fn python_provider_expected_fingerprint_includes_runtime_sha() {
    let matrix = provider_matrix::builtin_matrix();
    let entry = matrix
        .providers
        .iter()
        .find(|entry| entry.id == "kimi")
        .expect("kimi provider");

    let fingerprint =
        expected_managed_provider_artifact_fingerprint(entry, "1.38.0", InstallTarget::Host)
            .expect("expected fingerprint");

    assert!(fingerprint.contains("python=3.12.13"));
    assert!(fingerprint.contains("build=20260303"));
    assert!(
        fingerprint.contains("runtime_sha256="),
        "Python provider fingerprint must be bound to runtime content"
    );
}

#[test]
fn python_paths_for_container_target_use_linux_layout() {
    let python_root = Path::new("/tmp/python-runtime");
    let venv_root = Path::new("/tmp/provider-venv");
    assert_eq!(
        resolve_python_bin(python_root, InstallTarget::Container),
        python_root.join("bin").join("python")
    );
    assert_eq!(
        venv_exe(venv_root, "python", InstallTarget::Container),
        venv_root.join("bin").join("python")
    );
}

#[test]
fn python_paths_for_linux_targets_use_linux_layout() {
    let python_root = Path::new("/tmp/python-runtime");
    let venv_root = Path::new("/tmp/provider-venv");
    for target in [InstallTarget::LinuxAarch64, InstallTarget::LinuxX8664] {
        assert_eq!(
            resolve_python_bin(python_root, target),
            python_root.join("bin").join("python")
        );
        assert_eq!(
            venv_exe(venv_root, "python", target),
            venv_root.join("bin").join("python")
        );
    }
}

#[test]
fn validate_sha256_digest_accepts_case_insensitive_match() {
    assert!(validate_sha256_digest("ABcd1234", "abcd1234").is_ok());
}

#[test]
fn validate_sha256_digest_rejects_mismatch() {
    let err =
        validate_sha256_digest("abcd1234", "ffff1234").expect_err("mismatched digest should fail");
    assert!(err.to_string().contains("archive checksum mismatch"));
}

#[test]
fn agent_server_download_tmp_name_is_content_scoped() {
    let first = agent_server_download_tmp_name(
        "codex",
        "0.114.0-ctx.5",
        InstallTarget::Host,
        "https://example.invalid/codex-a.tar.gz",
        Some("A".repeat(64).as_str()),
    );
    let second = agent_server_download_tmp_name(
        "codex",
        "0.114.0-ctx.5",
        InstallTarget::Host,
        "https://example.invalid/codex-b.tar.gz",
        Some("B".repeat(64).as_str()),
    );
    assert_ne!(
        first, second,
        "downloads for the same provider/version/target but different content must not share tmp files"
    );
    assert!(
        first.contains("sha256-aaaaaaaa"),
        "expected digest should be visible in tmp identity: {first}"
    );
}

#[test]
fn resolve_download_resume_handles_partial_content() {
    let (resumed, total) =
        resolve_download_resume(120, reqwest::StatusCode::PARTIAL_CONTENT, Some(880));
    assert!(resumed);
    assert_eq!(total, Some(1000));
}

#[test]
fn resolve_download_resume_restarts_on_non_partial_status() {
    let (resumed, total) = resolve_download_resume(120, reqwest::StatusCode::OK, Some(880));
    assert!(!resumed);
    assert_eq!(total, Some(880));
}

#[test]
fn classify_install_error_maps_codes() {
    assert_eq!(
        classify_install_error("download", &anyhow::anyhow!("sending request failed")),
        InstallErrorCode::DownloadFailed
    );
    assert_eq!(
        classify_install_error("refresh", &anyhow::anyhow!("provider not healthy")),
        InstallErrorCode::HealthCheckFailed
    );
    assert_eq!(
        classify_install_error(
            "registry",
            &anyhow::anyhow!("managed install registry write failed")
        ),
        InstallErrorCode::RegistryWriteFailed
    );
    assert_eq!(
        classify_install_error("download", &anyhow::anyhow!("install canceled by user")),
        InstallErrorCode::Cancelled
    );
}

#[test]
fn apply_install_target_status_marks_mismatch_as_missing() {
    let mut status = status_with_managed_target("host");
    apply_install_target_status(&mut status, InstallTarget::Container);
    assert!(!status.installed);
    assert!(matches!(
        status.health,
        ctx_providers::adapters::ProviderHealth::Missing
    ));
    assert_eq!(
        status.details.get("target_mismatch").map(String::as_str),
        Some("true")
    );
}

#[test]
fn apply_install_target_status_marks_host_detected_status_unverified_for_container() {
    let mut status = host_detected_status();
    apply_install_target_status(&mut status, InstallTarget::Container);
    assert!(!status.installed);
    assert!(matches!(
        status.health,
        ctx_providers::adapters::ProviderHealth::Missing
    ));
    assert_eq!(
        status.details.get("target_unverified").map(String::as_str),
        Some("true")
    );
}

#[test]
fn validate_post_install_status_rejects_target_mismatch() {
    let status = status_with_managed_target("host");
    let err = validate_post_install_status(&status, "codex", InstallTarget::Container)
        .expect_err("target mismatch should fail verification");
    assert!(err.to_string().contains("expected 'container'"));
}

#[test]
fn validate_post_install_status_accepts_matching_target() {
    let status = status_with_managed_target("container");
    validate_post_install_status(&status, "codex", InstallTarget::Container)
        .expect("matching target should pass verification");
}

#[tokio::test]
async fn provider_install_lock_serializes_same_provider_target() {
    let first = acquire_provider_install_lock("codex", InstallTarget::Container).await;
    let acquired = Arc::new(AtomicBool::new(false));
    let acquired2 = acquired.clone();
    let waiter = tokio::spawn(async move {
        let _second = acquire_provider_install_lock("codex", InstallTarget::Container).await;
        acquired2.store(true, Ordering::SeqCst);
    });

    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(
        !acquired.load(Ordering::SeqCst),
        "second lock should block while first lock is held"
    );

    drop(first);
    tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("second lock should acquire after first unlock")
        .expect("waiter task should finish without panic");
    assert!(acquired.load(Ordering::SeqCst));
}

#[tokio::test]
async fn atomic_install_commit_replaces_existing_install_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let install_dir = temp.path().join("providers").join("codex").join("1.2.3");
    tokio::fs::create_dir_all(install_dir.join("old"))
        .await
        .expect("create old dir");
    tokio::fs::write(install_dir.join("old").join("keep.txt"), b"old")
        .await
        .expect("write old file");

    let staging_dir = prepare_atomic_install_dir(&install_dir)
        .await
        .expect("prepare staging dir");
    tokio::fs::create_dir_all(staging_dir.join("new"))
        .await
        .expect("create new dir");
    tokio::fs::write(staging_dir.join("new").join("fresh.txt"), b"new")
        .await
        .expect("write new file");

    commit_atomic_install_dir(&staging_dir, &install_dir)
        .await
        .expect("commit install dir");

    assert!(
        tokio::fs::metadata(staging_dir).await.is_err(),
        "staging dir should be moved into final location"
    );
    assert!(
        tokio::fs::metadata(install_dir.join("new").join("fresh.txt"))
            .await
            .is_ok()
    );
    assert!(
        tokio::fs::metadata(install_dir.join("old").join("keep.txt"))
            .await
            .is_err(),
        "old install contents should be replaced"
    );
}
