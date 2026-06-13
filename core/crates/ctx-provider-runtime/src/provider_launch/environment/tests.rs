use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Context;
use ctx_core::provider_ids::CODEX_PROVIDER_ID;

use super::mcp::apply_provider_mcp_command_overrides;
use super::openhands::should_fallback_to_direct_openhands_workdir;
use super::*;

#[tokio::test]
async fn opencode_launch_overrides_disable_mcp() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut env = HashMap::from([
        ("CTX_AUTH_TOKEN".to_string(), "daemon-secret".to_string()),
        ("CTX_MCP_TOKEN".to_string(), "mcp-secret".to_string()),
        (
            "CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN".to_string(),
            "shutdown-secret".to_string(),
        ),
    ]);

    apply_provider_launch_overrides("opencode", temp.path(), &mut env)
        .await
        .expect("apply overrides");

    assert_eq!(env.get("CTX_MCP_DISABLED").map(String::as_str), Some("1"));
    for key in ctx_core::env::DAEMON_AUTH_ENV_VARS {
        assert!(!env.contains_key(*key), "{key} should be stripped");
    }
    assert!(!env.contains_key("ACP_CWD"));
}

#[tokio::test]
async fn kimi_launch_overrides_disable_mcp() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut env = HashMap::from([
        ("CTX_AUTH_TOKEN".to_string(), "daemon-secret".to_string()),
        ("CTX_MCP_TOKEN".to_string(), "mcp-secret".to_string()),
        (
            "CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN".to_string(),
            "shutdown-secret".to_string(),
        ),
    ]);

    apply_provider_launch_overrides("kimi", temp.path(), &mut env)
        .await
        .expect("apply overrides");

    assert_eq!(env.get("CTX_MCP_DISABLED").map(String::as_str), Some("1"));
    for key in ctx_core::env::DAEMON_AUTH_ENV_VARS {
        assert!(!env.contains_key(*key), "{key} should be stripped");
    }
    assert!(!env.contains_key("ACP_CWD"));
}

#[test]
fn fake_provider_disables_mcp_before_command_resolution() {
    let mut env = HashMap::new();

    apply_provider_mcp_command_overrides("fake", &mut env);

    assert_eq!(env.get("CTX_MCP_DISABLED").map(String::as_str), Some("1"));
}

#[test]
fn broken_test_provider_disables_mcp_before_command_resolution() {
    let mut env = HashMap::new();

    apply_provider_mcp_command_overrides("broken", &mut env);

    assert_eq!(env.get("CTX_MCP_DISABLED").map(String::as_str), Some("1"));
}

#[tokio::test]
async fn unrelated_launch_overrides_leave_env_unchanged() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut env = HashMap::from([("CTX_MCP_DISABLED".to_string(), "0".to_string())]);

    apply_provider_launch_overrides(CODEX_PROVIDER_ID, temp.path(), &mut env)
        .await
        .expect("apply overrides");

    assert_eq!(env.get("CTX_MCP_DISABLED").map(String::as_str), Some("0"));
    assert!(!env.contains_key("ACP_CWD"));
}

#[tokio::test]
async fn predisabled_mcp_strips_unused_daemon_tokens() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut env = HashMap::from([
        ("CTX_MCP_DISABLED".to_string(), "1".to_string()),
        ("CTX_AUTH_TOKEN".to_string(), "daemon-secret".to_string()),
        ("CTX_MCP_TOKEN".to_string(), "mcp-secret".to_string()),
        (
            "CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN".to_string(),
            "shutdown-secret".to_string(),
        ),
    ]);

    apply_provider_launch_overrides("codex", temp.path(), &mut env)
        .await
        .expect("apply overrides");

    assert_eq!(env.get("CTX_MCP_DISABLED").map(String::as_str), Some("1"));
    for key in ctx_core::env::DAEMON_AUTH_ENV_VARS {
        assert!(!env.contains_key(*key), "{key} should be stripped");
    }
}

#[tokio::test]
async fn openhands_launch_overrides_set_short_workdir_alias() {
    let data_root = tempfile::tempdir().expect("data_root");
    let workdir_parent = tempfile::tempdir().expect("workdir_parent");
    let workdir = workdir_parent.path().join("nested").join("worktree");
    tokio::fs::create_dir_all(&workdir)
        .await
        .expect("create workdir");
    let mut env = HashMap::from([
        (
            "CTX_DATA_ROOT".to_string(),
            data_root.path().to_string_lossy().to_string(),
        ),
        ("CTX_SESSION_ID".to_string(), "session-123".to_string()),
    ]);

    apply_provider_launch_overrides("openhands", &workdir, &mut env)
        .await
        .expect("apply overrides");

    let alias = env
        .get("OPENHANDS_WORK_DIR")
        .map(PathBuf::from)
        .expect("OPENHANDS_WORK_DIR");
    assert_eq!(
        alias,
        data_root
            .path()
            .join("providers")
            .join("openhands")
            .join("workdir-aliases")
            .join("session-123")
    );
    let link_target = tokio::fs::read_link(&alias).await.expect("read alias");
    assert_eq!(link_target, workdir);
    assert!(!env.contains_key("CTX_MCP_DISABLED"));
}

#[test]
fn openhands_workdir_fallback_detects_windows_symlink_privilege_errors() {
    let privilege_err = Err::<(), _>(std::io::Error::from_raw_os_error(1314))
        .with_context(|| "symlink failed")
        .expect_err("expected privilege error");
    let unrelated_err = Err::<(), _>(std::io::Error::from_raw_os_error(5))
        .with_context(|| "symlink failed")
        .expect_err("expected unrelated error");

    assert!(should_fallback_to_direct_openhands_workdir(&privilege_err));
    assert!(!should_fallback_to_direct_openhands_workdir(&unrelated_err));
}
