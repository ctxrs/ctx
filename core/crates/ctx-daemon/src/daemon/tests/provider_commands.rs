use super::*;
use ctx_managed_installs as installer;
use ctx_provider_runtime::provider_launch::resolver::{
    normalize_acp_provider_command, runtime_probe_command_as_agent_command_for_target,
};

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

#[test]
fn normalizes_openhands_command_with_env_override_flag() {
    let temp = tempdir().unwrap();
    let input = installer::AgentServerCommand {
        command: "/tmp/openhands".to_string(),
        args: vec!["acp".to_string()],
        dependencies: Vec::new(),
        managed: None,
    };
    let normalized = normalize_acp_provider_command(temp.path(), "openhands", input)
        .expect("normalized openhands");
    assert_eq!(
        normalized.args,
        vec!["acp".to_string(), "--override-with-envs".to_string()]
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

    let resolved =
        runtime_probe_command_as_agent_command_for_target(temp.path(), &cfg, "cursor", None)
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
    assert_eq!(resolved.args[2], "--acp-command");
    assert!(resolved
        .args
        .get(3)
        .is_some_and(|arg| arg.ends_with("/cursor-agent --experimental-acp")));
    assert_eq!(
        resolved.dependencies,
        vec!["bridge-dep".to_string(), "cursor-dep".to_string()]
    );
}

#[test]
fn runtime_probe_command_keeps_native_crp_provider_unwrapped() {
    let temp = tempdir().unwrap();
    let codex_cmd = temp.path().join("codex");
    std::fs::write(&codex_cmd, b"codex").unwrap();
    let cfg = installer::AgentServerConfigFile {
        providers: HashMap::from([(
            "codex".to_string(),
            installer::AgentServerCommand {
                command: codex_cmd.to_string_lossy().to_string(),
                args: vec!["serve".to_string()],
                dependencies: vec!["codex-dep".to_string()],
                managed: None,
            },
        )]),
        provider_login_executables: HashMap::new(),
        provider_login_commands: HashMap::new(),
        managed_installs: HashMap::new(),
        managed_provider_targets: HashMap::new(),
        managed_install_targets: HashMap::new(),
    };

    let resolved =
        runtime_probe_command_as_agent_command_for_target(temp.path(), &cfg, "codex", None)
            .expect("probe command")
            .expect("runtime command");

    assert_eq!(
        PathBuf::from(&resolved.command)
            .file_name()
            .and_then(|name| name.to_str()),
        Some("codex")
    );
    assert_eq!(resolved.args, vec!["serve".to_string()]);
    assert_eq!(resolved.dependencies, vec!["codex-dep".to_string()]);
}
