use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use super::*;
use crate::{ProviderRuntime, ProviderRuntimeHost};

struct TestRuntimeHost {
    data_root: PathBuf,
    provider_runtime: ProviderRuntime,
}

impl TestRuntimeHost {
    fn new(data_root: PathBuf) -> Self {
        Self {
            data_root,
            provider_runtime: ProviderRuntime::new(HashMap::new()),
        }
    }
}

impl ProviderRuntimeHost for TestRuntimeHost {
    fn data_root(&self) -> &Path {
        &self.data_root
    }

    fn current_ctx_version(&self) -> Option<String> {
        None
    }

    fn provider_runtime(&self) -> &ProviderRuntime {
        &self.provider_runtime
    }
}

#[test]
fn normalizes_qwen_command_with_openai_auth_type() {
    let temp = tempdir().unwrap();
    let input = installer::AgentServerCommand {
        command: "/tmp/qwen".to_string(),
        args: vec!["--experimental-acp".to_string()],
        dependencies: Vec::new(),
        managed: None,
    };
    let normalized =
        normalize_acp_provider_command(temp.path(), "qwen", input).expect("normalized qwen");
    assert_eq!(
        normalized.args,
        vec![
            "--experimental-acp".to_string(),
            "--auth-type".to_string(),
            "openai".to_string(),
        ]
    );
}

#[tokio::test]
async fn ensure_provider_adapter_for_target_surfaces_agent_server_config_errors_without_caching() {
    let data_root = tempfile::tempdir().expect("tempdir");
    let config_path = installer::agent_server_config_path(data_root.path());
    std::fs::create_dir_all(config_path.parent().expect("config parent")).expect("mkdir");
    std::fs::write(&config_path, "{ not valid json").expect("write invalid config");

    let host = TestRuntimeHost::new(data_root.path().to_path_buf());
    let err =
        match ensure_provider_adapter_for_target(&host, "codex", InstallTarget::Container).await {
            Ok(_) => panic!("invalid managed config should fail adapter resolution"),
            Err(err) => err,
        };

    assert!(err.to_string().contains("loading agent server config"));
    assert!(
        host.provider_runtime
            .target_provider_adapter("codex@container")
            .await
            .is_none(),
        "invalid managed config should not seed target adapter cache"
    );
    assert!(
        host.provider_runtime
            .provider_adapter("codex")
            .await
            .is_none(),
        "invalid managed config should not seed provider adapter cache"
    );
}

#[test]
fn normalizes_goose_command_with_acp_and_developer_builtin() {
    let temp = tempdir().unwrap();
    let input = installer::AgentServerCommand {
        command: "/tmp/goose".to_string(),
        args: Vec::new(),
        dependencies: Vec::new(),
        managed: None,
    };
    let normalized =
        normalize_acp_provider_command(temp.path(), "goose", input).expect("normalized goose");
    assert_eq!(
        normalized.args,
        vec![
            "acp".to_string(),
            "--with-builtin".to_string(),
            "developer".to_string(),
        ]
    );
}

#[test]
fn preserves_existing_goose_developer_builtin() {
    let temp = tempdir().unwrap();
    let input = installer::AgentServerCommand {
        command: "/tmp/goose".to_string(),
        args: vec![
            "acp".to_string(),
            "--with-builtin".to_string(),
            "developer,computercontroller".to_string(),
        ],
        dependencies: Vec::new(),
        managed: None,
    };
    let normalized =
        normalize_acp_provider_command(temp.path(), "goose", input).expect("normalized goose");
    assert_eq!(
        normalized.args,
        vec![
            "acp".to_string(),
            "--with-builtin".to_string(),
            "developer,computercontroller".to_string(),
        ]
    );
}

#[test]
fn classifies_upstream_openhands_runtime_contract() {
    let cmd = installer::AgentServerCommand {
        command: "/tmp/openhands".to_string(),
        args: vec!["acp".to_string(), "--override-with-envs".to_string()],
        dependencies: Vec::new(),
        managed: None,
    };

    assert_eq!(
        openhands_runtime_contract_for_command(&cmd),
        OpenHandsRuntimeContract::UpstreamAcp
    );
}

#[test]
fn classifies_legacy_openhands_shim_runtime_contract() {
    let cmd = installer::AgentServerCommand {
        command: "/tmp/openhands-acp.js".to_string(),
        args: Vec::new(),
        dependencies: Vec::new(),
        managed: None,
    };

    assert_eq!(
        openhands_runtime_contract_for_command(&cmd),
        OpenHandsRuntimeContract::ShimAcp
    );
}

#[tokio::test]
async fn openhands_bridge_adapter_inspect_surfaces_runtime_contract_details() {
    let temp = tempdir().unwrap();
    let bridge_cmd = temp.path().join("acp-crp-bridge");
    std::fs::write(&bridge_cmd, b"bridge").unwrap();

    let adapter = acp_bridge_adapter(
        "openhands",
        &installer::AgentServerCommand {
            command: bridge_cmd.to_string_lossy().to_string(),
            args: vec!["--stdio".to_string()],
            dependencies: Vec::new(),
            managed: None,
        },
        installer::AgentServerCommand {
            command: "/tmp/openhands-acp.js".to_string(),
            args: Vec::new(),
            dependencies: Vec::new(),
            managed: None,
        },
    );

    let status = adapter.inspect().await.expect("inspect status");
    assert_eq!(
        status
            .details
            .get("openhands_runtime_contract")
            .map(String::as_str),
        Some("shim_acp")
    );
    assert_eq!(
        status
            .details
            .get("openhands_real_runtime")
            .map(String::as_str),
        Some("false")
    );
    assert_eq!(
        status
            .details
            .get("openhands_runtime_contract_note")
            .map(String::as_str),
        Some("runtime command still points at the legacy `openhands-acp` shim")
    );
}

fn create_gemini_runtime_layout(root: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
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
        .expect("bundle dir")
        .parent()
        .expect("gemini cli root")
        .join("package.json");
    let core_entry = cli_entry
        .parent()
        .expect("bundle dir")
        .join("core-ctx-test.js");
    std::fs::create_dir_all(node_bin.parent().unwrap()).unwrap();
    std::fs::create_dir_all(cli_entry.parent().unwrap()).unwrap();
    std::fs::write(&node_bin, b"node").unwrap();
    std::fs::write(&cli_entry, b"cli").unwrap();
    std::fs::write(
        &core_entry,
        "export const coreEvents = {}; export const CoreEvent = {}; export const writeToStdout = () => {}; export const writeToStderr = () => {};",
    )
    .unwrap();
    std::fs::write(
        &package_json,
        r#"{"name":"@google/gemini-cli","version":"0.38.2"}"#,
    )
    .unwrap();
    (node_bin, cli_entry, package_json, core_entry)
}

#[test]
fn accepts_explicit_gemini_node_entrypoint_for_acp() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let (node_bin, cli_entry, _, core_entry) = create_gemini_runtime_layout(temp.path());

    let input = installer::AgentServerCommand {
        command: node_bin.to_string_lossy().to_string(),
        args: vec![
            cli_entry.to_string_lossy().to_string(),
            "--experimental-acp".to_string(),
        ],
        dependencies: Vec::new(),
        managed: None,
    };
    let wrapped =
        normalize_acp_provider_command(&data_root, "gemini", input).expect("wrapped gemini");

    assert_eq!(wrapped.command, node_bin.to_string_lossy().to_string());
    let wrapper_path = data_root
        .join("providers")
        .join("agent-servers")
        .join("gemini-acp-wrapper.mjs");
    assert_eq!(
        wrapped.args.first().map(String::as_str),
        wrapper_path.to_str()
    );
    assert_eq!(
        wrapped.args.get(1).map(String::as_str),
        Some("--experimental-acp")
    );
    let wrapper = std::fs::read_to_string(&wrapper_path).expect("read Gemini wrapper");
    assert!(
        wrapper.contains(core_entry.to_string_lossy().as_ref()),
        "wrapper should import the bundled Gemini core entrypoint"
    );
    assert!(
        wrapper.contains(cli_entry.to_string_lossy().as_ref()),
        "wrapper should import the bundled Gemini CLI entrypoint"
    );
    assert!(
        wrapper.contains("CoreEvent.ConsentRequest"),
        "wrapper should auto-confirm Gemini consent requests"
    );
    assert!(
        wrapper.contains("GEMINI_CLI_NO_RELAUNCH"),
        "wrapper should suppress Gemini self-relaunch in ACP mode"
    );
}

#[test]
fn rejects_path_style_gemini_command() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let gemini_bin = temp.path().join("bundle").join("bin").join("gemini");
    std::fs::create_dir_all(gemini_bin.parent().unwrap()).unwrap();
    std::fs::write(&gemini_bin, b"gemini").unwrap();

    let input = installer::AgentServerCommand {
        command: gemini_bin.to_string_lossy().to_string(),
        args: vec!["--experimental-acp".to_string()],
        dependencies: Vec::new(),
        managed: None,
    };
    let err = normalize_acp_provider_command(&data_root, "gemini", input).unwrap_err();

    assert!(err
        .to_string()
        .contains("must use an explicit absolute node executable"));
}

#[test]
fn rejects_relative_gemini_entrypoint() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let (node_bin, _, _, _) = create_gemini_runtime_layout(temp.path());

    let input = installer::AgentServerCommand {
        command: node_bin.to_string_lossy().to_string(),
        args: vec![
            "node_modules/@google/gemini-cli/bundle/gemini.js".to_string(),
            "--experimental-acp".to_string(),
        ],
        dependencies: Vec::new(),
        managed: None,
    };
    let err = normalize_acp_provider_command(&data_root, "gemini", input).unwrap_err();

    assert!(err
        .to_string()
        .contains("Gemini ACP entrypoint must be an absolute path"));
}

#[test]
fn rejects_gemini_runtime_when_package_json_is_missing() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let (node_bin, cli_entry, package_json, _) = create_gemini_runtime_layout(temp.path());
    std::fs::remove_file(package_json).unwrap();

    let input = installer::AgentServerCommand {
        command: node_bin.to_string_lossy().to_string(),
        args: vec![
            cli_entry.to_string_lossy().to_string(),
            "--experimental-acp".to_string(),
        ],
        dependencies: Vec::new(),
        managed: None,
    };
    let err = normalize_acp_provider_command(&data_root, "gemini", input).unwrap_err();

    assert!(err.to_string().contains(
        "Gemini ACP entrypoint must live under a node_modules/@google/gemini-cli install tree"
    ));
}

#[test]
fn rejects_gemini_runtime_when_bundled_core_entry_is_missing() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let (node_bin, cli_entry, _, core_entry) = create_gemini_runtime_layout(temp.path());
    std::fs::remove_file(core_entry).unwrap();

    let input = installer::AgentServerCommand {
        command: node_bin.to_string_lossy().to_string(),
        args: vec![
            cli_entry.to_string_lossy().to_string(),
            "--experimental-acp".to_string(),
        ],
        dependencies: Vec::new(),
        managed: None,
    };
    let err = normalize_acp_provider_command(&data_root, "gemini", input).unwrap_err();

    assert!(err
        .to_string()
        .contains("Gemini ACP bundled core entrypoint is missing"));
}

#[test]
fn rejects_gemini_runtime_outside_node_modules_install_tree() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let node_bin = temp
        .path()
        .join("bundle")
        .join("runtimes")
        .join("node")
        .join("bin")
        .join("node");
    let cli_entry = temp
        .path()
        .join("bundle")
        .join("providers")
        .join("gemini")
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
        .expect("bundle dir")
        .parent()
        .expect("gemini cli root")
        .join("package.json");
    std::fs::create_dir_all(node_bin.parent().unwrap()).unwrap();
    std::fs::create_dir_all(cli_entry.parent().unwrap()).unwrap();
    std::fs::write(&node_bin, b"node").unwrap();
    std::fs::write(&cli_entry, b"cli").unwrap();
    std::fs::write(
        &core_entry,
        "export const coreEvents = {}; export const CoreEvent = {}; export const writeToStdout = () => {}; export const writeToStderr = () => {};",
    )
    .unwrap();
    std::fs::write(
        &package_json,
        r#"{"name":"@google/gemini-cli","version":"0.38.2"}"#,
    )
    .unwrap();

    let input = installer::AgentServerCommand {
        command: node_bin.to_string_lossy().to_string(),
        args: vec![
            cli_entry.to_string_lossy().to_string(),
            "--experimental-acp".to_string(),
        ],
        dependencies: Vec::new(),
        managed: None,
    };
    let err = normalize_acp_provider_command(&data_root, "gemini", input).unwrap_err();

    assert!(err
        .to_string()
        .contains("Gemini ACP entrypoint must point to @google/gemini-cli/bundle/gemini.js"));
}

#[test]
fn accepts_gemini_runtime_when_bundle_has_multiple_core_entries() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let (node_bin, cli_entry, _, core_entry) = create_gemini_runtime_layout(temp.path());
    let extra_core_entry = cli_entry.parent().expect("bundle dir").join("core-zeta.js");
    std::fs::write(
        &extra_core_entry,
        "export const coreEvents = {}; export const CoreEvent = {}; export const writeToStdout = () => {}; export const writeToStderr = () => {};",
    )
    .unwrap();
    assert!(core_entry.exists());

    let input = installer::AgentServerCommand {
        command: node_bin.to_string_lossy().to_string(),
        args: vec![
            cli_entry.to_string_lossy().to_string(),
            "--experimental-acp".to_string(),
        ],
        dependencies: Vec::new(),
        managed: None,
    };

    normalize_acp_provider_command(&data_root, "gemini", input).expect("wrapped gemini");
    let wrapper_path = data_root
        .join("providers")
        .join("agent-servers")
        .join("gemini-acp-wrapper.mjs");
    let wrapper = std::fs::read_to_string(&wrapper_path).expect("read Gemini wrapper");
    assert!(
        wrapper.contains(core_entry.to_string_lossy().as_ref()),
        "wrapper should import the first bundled Gemini core candidate"
    );
    assert!(
        wrapper.contains(extra_core_entry.to_string_lossy().as_ref()),
        "wrapper should import the extra bundled Gemini core candidate"
    );
    assert!(
        wrapper.contains("coreCandidates.find"),
        "wrapper should select the compatible Gemini core module at runtime"
    );
}

#[test]
fn runtime_probe_command_wraps_acp_provider_with_bridge() {
    let temp = tempdir().unwrap();
    let cursor_cmd = temp.path().join("cursor-agent");
    let bridge_cmd = temp.path().join("acp-crp-bridge");
    std::fs::write(&cursor_cmd, b"cursor").unwrap();
    std::fs::write(&bridge_cmd, b"bridge").unwrap();
    let cfg = installer::AgentServerConfigFile {
        providers: HashMap::from([
            (
                "cursor".to_string(),
                installer::AgentServerCommand {
                    command: cursor_cmd.to_string_lossy().to_string(),
                    args: vec!["--experimental-acp".to_string()],
                    dependencies: vec!["cursor-dep".to_string()],
                    managed: None,
                },
            ),
            (
                "acp-crp-bridge".to_string(),
                installer::AgentServerCommand {
                    command: bridge_cmd.to_string_lossy().to_string(),
                    args: vec!["--log-level".to_string(), "debug".to_string()],
                    dependencies: vec!["bridge-dep".to_string()],
                    managed: None,
                },
            ),
        ]),
        provider_login_executables: HashMap::new(),
        provider_login_commands: HashMap::new(),
        managed_installs: HashMap::new(),
        managed_provider_targets: HashMap::new(),
        managed_install_targets: HashMap::new(),
    };

    let resolved = runtime_probe_command_as_agent_command(temp.path(), &cfg, "cursor")
        .expect("probe command")
        .expect("runtime command");

    assert_eq!(
        PathBuf::from(&resolved.command)
            .file_name()
            .and_then(|name| name.to_str()),
        Some("acp-crp-bridge")
    );
    assert_eq!(resolved.args.len(), 4);
    assert_eq!(resolved.args[0], "--log-level");
    assert_eq!(resolved.args[1], "debug");
    assert_eq!(resolved.dependencies, vec!["bridge-dep", "cursor-dep"]);
}

#[test]
fn runtime_probe_command_rejects_path_style_gemini_runtime() {
    let temp = tempdir().unwrap();
    let gemini_bin = temp.path().join("bundle").join("bin").join("gemini");
    let bridge_cmd = temp.path().join("acp-crp-bridge");
    std::fs::create_dir_all(gemini_bin.parent().unwrap()).unwrap();
    std::fs::write(&gemini_bin, b"gemini").unwrap();
    std::fs::write(&bridge_cmd, b"bridge").unwrap();
    let cfg = installer::AgentServerConfigFile {
        providers: HashMap::from([
            (
                "gemini".to_string(),
                installer::AgentServerCommand {
                    command: gemini_bin.to_string_lossy().to_string(),
                    args: vec!["--experimental-acp".to_string()],
                    dependencies: Vec::new(),
                    managed: None,
                },
            ),
            (
                "acp-crp-bridge".to_string(),
                installer::AgentServerCommand {
                    command: bridge_cmd.to_string_lossy().to_string(),
                    args: vec!["--log-level".to_string(), "debug".to_string()],
                    dependencies: Vec::new(),
                    managed: None,
                },
            ),
        ]),
        provider_login_executables: HashMap::new(),
        provider_login_commands: HashMap::new(),
        managed_installs: HashMap::new(),
        managed_provider_targets: HashMap::new(),
        managed_install_targets: HashMap::new(),
    };

    let err = runtime_probe_command_as_agent_command(temp.path(), &cfg, "gemini").unwrap_err();
    assert!(err
        .to_string()
        .contains("must use an explicit absolute node executable"));
}
