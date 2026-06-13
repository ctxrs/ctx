use super::*;
use std::fs;

#[test]
fn crp_event_broadcast_capacity_covers_remote_soak_burst() {
    assert!(
        CRP_EVENT_BROADCAST_CAPACITY >= 16_384,
        "CRP prompt subscribers should not lag on ordinary remote-soak burst sizes"
    );
    assert_eq!(CRP_STDERR_BROADCAST_CAPACITY, 256);

    let calibrated_burst = 8_192usize;
    let (tx, mut rx) = tokio::sync::broadcast::channel(CRP_EVENT_BROADCAST_CAPACITY);
    for seq in 0..calibrated_burst {
        tx.send(seq).expect("broadcast send");
    }
    for expected in 0..calibrated_burst {
        assert_eq!(rx.try_recv().expect("receiver should not lag"), expected);
    }
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
}

#[test]
fn redact_sensitive_covers_scoped_mcp_tokens() {
    let redacted = redact_sensitive(
        r#"stderr {"env":{"CTX_MCP_TOKEN":"ctxmcp_secret","ctx_mcp_token": "ctxmcp_lower","CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN":"shutdown_json"}} CTX_MCP_TOKEN=ctxmcp_env CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN=shutdown_env"#,
    );

    assert!(!redacted.contains("ctxmcp_secret"));
    assert!(!redacted.contains("ctxmcp_lower"));
    assert!(!redacted.contains("ctxmcp_env"));
    assert!(!redacted.contains("shutdown_json"));
    assert!(!redacted.contains("shutdown_env"));
    assert_eq!(redacted.matches("[REDACTED]").count(), 5);
}

#[test]
fn container_exec_outer_process_env_skips_provider_home_and_xdg_keys() {
    assert!(should_skip_outer_process_env_key("HOME", true));
    assert!(should_skip_outer_process_env_key("TMPDIR", true));
    assert!(should_skip_outer_process_env_key("XDG_CONFIG_HOME", true));
    assert!(should_skip_outer_process_env_key("XDG_STATE_HOME", true));
    assert!(!should_skip_outer_process_env_key("OPENAI_API_KEY", true));
    assert!(!should_skip_outer_process_env_key("HOME", false));
}

#[test]
fn prepare_crp_spawn_env_projects_dump_paths_into_shared_vm_container_exec_env() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_root = tmp.path().join("data-root");
    let container_data_root = data_root
        .join("containers")
        .join("workspaces")
        .join("workspace-1")
        .join("data");
    let host_worktree_root = data_root.join("worktrees/ws/wt");
    fs::create_dir_all(&host_worktree_root).expect("mkdir host worktree");

    let mut env = HashMap::new();
    env.insert(
        "CTX_DATA_ROOT_HOST".to_string(),
        data_root.to_string_lossy().to_string(),
    );
    env.insert(
        "CTX_DATA_ROOT".to_string(),
        container_data_root.to_string_lossy().to_string(),
    );
    env.insert(
        "CTX_HARNESS_RUNTIME_KIND".to_string(),
        "shared_vm_container".to_string(),
    );
    env.insert(
        "CTX_AVF_LINUX_HELPER_PATH".to_string(),
        "/usr/local/bin/ctx-avf-linux-helper".to_string(),
    );
    env.insert(
        "CTX_AVF_HOST_DATA_ROOT".to_string(),
        data_root.to_string_lossy().to_string(),
    );
    env.insert("CTX_AVF_REAL_GUEST_EXEC".to_string(), "1".to_string());
    env.insert(
        "CTX_AVF_WORKSPACE_ID".to_string(),
        "workspace-1".to_string(),
    );
    env.insert("CTX_AVF_WORKTREE_ID".to_string(), "worktree-1".to_string());
    env.insert(
        "CTX_AVF_HOST_WORKTREE_ROOT".to_string(),
        host_worktree_root.to_string_lossy().to_string(),
    );
    env.insert(
        "CTX_AVF_GUEST_WORKTREE_ROOT".to_string(),
        "/ctx/ws/worktrees/worktree-1".to_string(),
    );
    env.insert(
        "CTX_HARNESS_GUEST_WORKSPACE_ROOT".to_string(),
        "/ctx/ws".to_string(),
    );

    let prepared = prepare_crp_spawn_env(&env, "codex");
    let codex_dump = prepared
        .env
        .get(CODEX_CRP_DUMP_CODEX_EVENTS_ENV)
        .expect("codex dump path");
    let crp_dump = prepared
        .env
        .get(CODEX_CRP_DUMP_CRP_EVENTS_ENV)
        .expect("crp dump path");
    assert!(
        codex_dump.starts_with(&format!(
            "{}/logs/providers/crp-codex-",
            container_data_root.display()
        )),
        "shared VM container child must receive container-visible dump path, got {codex_dump}"
    );
    assert!(
        crp_dump.starts_with(&format!(
            "{}/logs/providers/crp-codex-",
            container_data_root.display()
        )),
        "shared VM container child must receive container-visible dump path, got {crp_dump}"
    );
    let raw_stdout = prepared
        .raw_stdout_log_path
        .as_ref()
        .expect("raw stdout log path");
    assert!(
        raw_stdout.starts_with(data_root.join("logs").join("providers")),
        "parent stdout pump should keep host-side log path, got {}",
        raw_stdout.display()
    );
    assert!(
        prepared.raw_stdout_log_path.is_some(),
        "parent stdout pump should capture a host-side raw stdout log"
    );
    assert!(
        prepared.stderr_log_path.is_some(),
        "parent stderr pump should capture a host-side stderr log"
    );

    let spec = crate::container_exec::container_exec_spec(&prepared.env)
        .expect("shared VM container exec spec");
    let cmd = crate::container_exec::build_container_exec_command(
        &spec,
        &host_worktree_root,
        &prepared.env,
        "/usr/local/bin/codex-crp",
        &[],
    )
    .expect("build shared VM exec command");
    let args = cmd
        .as_std()
        .get_args()
        .map(|arg| arg.to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert!(
        args.windows(2).any(|window| {
            window[0] == "--env" && window[1].starts_with(CODEX_CRP_DUMP_CODEX_EVENTS_ENV)
        }),
        "container exec env args must include Codex app-server dump path: {args:?}"
    );
    assert!(
        args.windows(2).any(|window| {
            window[0] == "--env" && window[1].starts_with(CODEX_CRP_DUMP_CRP_EVENTS_ENV)
        }),
        "container exec env args must include CRP dump path: {args:?}"
    );
}

#[test]
fn rewrite_bundled_path_for_linux_rewrites_provider_paths() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let host = tmp
        .path()
        .join("bundles/providers/acp-crp-bridge/macos/aarch64/acp-crp-bridge");
    let linux = tmp
        .path()
        .join("bundles/providers/acp-crp-bridge/linux/aarch64/acp-crp-bridge");
    fs::create_dir_all(linux.parent().expect("parent")).expect("mkdir");
    fs::write(&linux, b"ok").expect("write");

    let rewritten = rewrite_bundled_path_for_linux(host.to_string_lossy().as_ref())
        .expect("rewrite should succeed");
    assert_eq!(rewritten, linux.to_string_lossy());
}

#[test]
fn rewrite_bundled_path_for_linux_rewrites_e2e_bundle_provider_paths() {
    let tmp = tempfile::Builder::new()
        .prefix("ctx-e2e-bundles-runtime-probe-")
        .tempdir()
        .expect("tempdir");
    let host = tmp.path().join("providers/codex/macos/aarch64/codex-crp");
    let linux = tmp.path().join("providers/codex/linux/aarch64/codex-crp");
    fs::create_dir_all(linux.parent().expect("parent")).expect("mkdir");
    fs::write(&linux, b"ok").expect("write linux");
    fs::write(tmp.path().join("manifest.json"), "{}").expect("write manifest");

    let rewritten = rewrite_bundled_path_for_linux(host.to_string_lossy().as_ref())
        .expect("rewrite should succeed");
    assert_eq!(rewritten, linux.to_string_lossy());
}

#[test]
fn rewrite_bundled_path_for_linux_rewrites_runtime_flavor_directory() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let host = tmp
        .path()
        .join("bundles/runtimes/node/macos/aarch64/node-v24.12.0-darwin-arm64/bin/node");
    let linux = tmp
        .path()
        .join("bundles/runtimes/node/linux/aarch64/node-v24.12.0-linux-arm64/bin/node");
    fs::create_dir_all(linux.parent().expect("parent")).expect("mkdir");
    fs::write(&linux, b"ok").expect("write");
    let manifest_path = tmp.path().join("bundles/manifest.json");
    fs::write(
        &manifest_path,
        serde_json::json!({
            "version": 1,
            "providers": [],
            "runtimes": [
                {
                    "id": "node",
                    "os": "linux",
                    "arch": "aarch64",
                    "root": "runtimes/node/linux/aarch64/node-v24.12.0-linux-arm64",
                    "bin": "bin/node"
                }
            ],
            "images": [],
            "daemons": []
        })
        .to_string(),
    )
    .expect("write manifest");

    let rewritten = rewrite_bundled_path_for_linux(host.to_string_lossy().as_ref())
        .expect("rewrite should succeed");
    assert_eq!(rewritten, linux.to_string_lossy());
}

#[test]
fn rewrite_bundled_path_for_linux_rewrites_runtime_flavor_directory_for_e2e_bundle_root() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let host = tmp
        .path()
        .join("runtimes/node/macos/aarch64/node-v24.12.0-darwin-arm64/bin/node");
    let linux = tmp
        .path()
        .join("runtimes/node/linux/aarch64/node-v24.12.0-linux-arm64/bin/node");
    fs::create_dir_all(linux.parent().expect("parent")).expect("mkdir");
    fs::write(&linux, b"ok").expect("write");
    let manifest_path = tmp.path().join("manifest.json");
    fs::write(
        &manifest_path,
        serde_json::json!({
            "version": 1,
            "providers": [],
            "runtimes": [
                {
                    "id": "node",
                    "os": "linux",
                    "arch": "aarch64",
                    "root": "runtimes/node/linux/aarch64/node-v24.12.0-linux-arm64",
                    "bin": "bin/node"
                }
            ],
            "images": [],
            "daemons": []
        })
        .to_string(),
    )
    .expect("write manifest");

    let rewritten = rewrite_bundled_path_for_linux(host.to_string_lossy().as_ref())
        .expect("rewrite should succeed");
    assert_eq!(rewritten, linux.to_string_lossy());
}

#[test]
fn rewrite_container_args_for_linux_rewrites_nested_acp_command_paths() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let host_provider = tmp
        .path()
        .join("bundles/providers/pi/macos/aarch64/pi-acp.js");
    let linux_provider = tmp
        .path()
        .join("bundles/providers/pi/linux/aarch64/pi-acp.js");
    let host_node = tmp
        .path()
        .join("bundles/runtimes/node/macos/aarch64/node-v1/bin/node");
    let linux_node = tmp
        .path()
        .join("bundles/runtimes/node/linux/aarch64/node-v1/bin/node");
    fs::create_dir_all(linux_provider.parent().expect("parent")).expect("mkdir");
    fs::create_dir_all(host_node.parent().expect("parent")).expect("mkdir host node");
    fs::create_dir_all(linux_node.parent().expect("parent")).expect("mkdir");
    fs::write(&linux_provider, b"ok").expect("write");
    fs::write(&host_node, b"ok").expect("write host node");
    fs::write(&linux_node, b"ok").expect("write");

    let raw_acp = format!("{} --foo", host_provider.to_string_lossy());
    let args = vec!["--acp-command".to_string(), raw_acp];
    let mut env = HashMap::new();
    env.insert(
        "PATH".to_string(),
        host_node
            .parent()
            .expect("node dir")
            .to_string_lossy()
            .to_string(),
    );
    let rewritten = rewrite_container_args_for_linux(&args, &env).expect("rewrite args");
    assert_eq!(rewritten.len(), 2);
    let parsed = shlex::split(&rewritten[1]).expect("parse rewritten command");
    assert_eq!(
        parsed,
        vec![
            linux_node.to_string_lossy().to_string(),
            linux_provider.to_string_lossy().to_string(),
            "--foo".to_string(),
        ]
    );
}

#[test]
fn rewrite_container_args_for_linux_preserves_quoted_paths_with_spaces() {
    let tmp = tempfile::Builder::new()
        .prefix("ctx bundles with spaces ")
        .tempdir()
        .expect("tempdir");
    let host_provider = tmp
        .path()
        .join("bundles/providers/pi/macos/aarch64/pi-acp.js");
    let linux_provider = tmp
        .path()
        .join("bundles/providers/pi/linux/aarch64/pi-acp.js");
    let host_node = tmp
        .path()
        .join("bundles/runtimes/node/macos/aarch64/node-v1/bin/node");
    let linux_node = tmp
        .path()
        .join("bundles/runtimes/node/linux/aarch64/node-v1/bin/node");
    fs::create_dir_all(linux_provider.parent().expect("parent")).expect("mkdir");
    fs::create_dir_all(host_node.parent().expect("parent")).expect("mkdir host node");
    fs::create_dir_all(linux_node.parent().expect("parent")).expect("mkdir");
    fs::write(&linux_provider, b"ok").expect("write");
    fs::write(&host_node, b"ok").expect("write host node");
    fs::write(&linux_node, b"ok").expect("write");

    let raw_acp = shlex::try_join(
        [
            host_provider.to_string_lossy().to_string(),
            "--flag".to_string(),
        ]
        .iter()
        .map(String::as_str),
    )
    .expect("quote acp command");
    let args = vec!["--acp-command".to_string(), raw_acp];
    let mut env = HashMap::new();
    env.insert(
        "PATH".to_string(),
        host_node
            .parent()
            .expect("node dir")
            .to_string_lossy()
            .to_string(),
    );
    let rewritten = rewrite_container_args_for_linux(&args, &env).expect("rewrite args");
    assert_eq!(rewritten.len(), 2);
    let parsed = shlex::split(&rewritten[1]).expect("parse rewritten command");
    assert_eq!(
        parsed,
        vec![
            linux_node.to_string_lossy().to_string(),
            linux_provider.to_string_lossy().to_string(),
            "--flag".to_string(),
        ]
    );
}

#[test]
fn rewrite_container_args_for_linux_keeps_explicit_node_binary_for_acp_command() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let host_provider = tmp
        .path()
        .join("bundles/providers/pi/macos/aarch64/pi-acp.js");
    let linux_provider = tmp
        .path()
        .join("bundles/providers/pi/linux/aarch64/pi-acp.js");
    let host_node = tmp
        .path()
        .join("bundles/runtimes/node/macos/aarch64/node-v1/bin/node");
    let linux_node = tmp
        .path()
        .join("bundles/runtimes/node/linux/aarch64/node-v1/bin/node");
    fs::create_dir_all(linux_provider.parent().expect("parent")).expect("mkdir");
    fs::create_dir_all(linux_node.parent().expect("parent")).expect("mkdir");
    fs::write(&linux_provider, b"ok").expect("write provider");
    fs::write(&linux_node, b"ok").expect("write node");

    let raw_acp = shlex::try_join(
        [
            host_node.to_string_lossy().to_string(),
            host_provider.to_string_lossy().to_string(),
            "--flag".to_string(),
        ]
        .iter()
        .map(String::as_str),
    )
    .expect("quote acp command");
    let args = vec!["--acp-command".to_string(), raw_acp];
    let mut env = HashMap::new();
    env.insert(
        "PATH".to_string(),
        linux_node
            .parent()
            .expect("node dir")
            .to_string_lossy()
            .to_string(),
    );

    let rewritten = rewrite_container_args_for_linux(&args, &env).expect("rewrite args");
    let parsed = shlex::split(&rewritten[1]).expect("parse rewritten command");
    assert_eq!(
        parsed,
        vec![
            linux_node.to_string_lossy().to_string(),
            linux_provider.to_string_lossy().to_string(),
            "--flag".to_string(),
        ]
    );
}

#[test]
fn rewrite_container_command_for_linux_uses_explicit_node_binary_for_js_entrypoints() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let node_dir = tmp.path().join("runtimes/node/linux/aarch64/node-v1/bin");
    fs::create_dir_all(&node_dir).expect("mkdir node dir");
    let node_bin = node_dir.join("node");
    fs::write(&node_bin, b"ok").expect("write node");
    let script = tmp.path().join("providers/pi/linux/aarch64/pi-acp.js");
    fs::create_dir_all(script.parent().expect("parent")).expect("mkdir script parent");
    fs::write(&script, b"#!/usr/bin/env node\n").expect("write script");

    let mut env = HashMap::new();
    env.insert("PATH".to_string(), node_dir.to_string_lossy().to_string());
    let args = vec!["--flag".to_string()];

    let (command, rewritten_args) =
        rewrite_container_command_for_linux(script.to_string_lossy().as_ref(), &args, &env)
            .expect("rewrite command");

    assert_eq!(command, node_bin.to_string_lossy());
    assert_eq!(
        rewritten_args,
        vec![script.to_string_lossy().to_string(), "--flag".to_string()]
    );
}

#[test]
fn rewrite_bundled_path_for_linux_errors_when_linux_target_missing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let host = tmp
        .path()
        .join("bundles/providers/cursor/macos/aarch64/cursor-agent-acp.js");
    fs::create_dir_all(host.parent().expect("parent")).expect("mkdir");
    fs::write(&host, b"ok").expect("write");

    let err = rewrite_bundled_path_for_linux(host.to_string_lossy().as_ref())
        .expect_err("expected missing linux target error");
    let msg = err.to_string();
    assert!(msg.contains("missing linux bundled path"));
}

#[test]
fn rewrite_bundled_path_for_linux_ignores_managed_install_provider_paths() {
    let path =
        "/tmp/providers/agent-servers/cursor-agent-acp/node_modules/@scope/pkg/dist/bin/app.js";
    let rewritten = rewrite_bundled_path_for_linux(path).expect("rewrite should succeed");
    assert_eq!(rewritten, path);
}

#[test]
fn rewrite_bundled_path_for_linux_ignores_managed_install_runtime_paths() {
    let path = "/tmp/runtimes/node/v24.12.0/bin/node";
    let rewritten = rewrite_bundled_path_for_linux(path).expect("rewrite should succeed");
    assert_eq!(rewritten, path);
}

#[test]
fn rewrite_container_args_for_linux_rejects_invalid_shell_command() {
    let args = vec!["--acp-command".to_string(), "\"unterminated".to_string()];
    let err =
        rewrite_container_args_for_linux(&args, &HashMap::new()).expect_err("expected parse error");
    assert!(err
        .to_string()
        .contains("invalid shell command in --acp-command"));
}
