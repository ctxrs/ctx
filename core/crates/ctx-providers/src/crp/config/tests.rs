use super::*;
use base64::Engine as _;
use std::fs;
use std::path::{Path, PathBuf};

fn disable_ctx_mcp(env: &mut HashMap<String, String>) {
    env.insert("CTX_MCP_DISABLED".to_string(), "1".to_string());
}

fn write_test_ctx_mcp_command(root: &Path) -> String {
    let command = root.join("ctx-mcp");
    fs::write(&command, b"#!/bin/sh\n").expect("write ctx-mcp");
    command.to_string_lossy().to_string()
}

#[test]
fn probe_timeout_for_env_defaults_to_host_timeout() {
    let env = HashMap::<String, String>::new();
    assert_eq!(
        probe_timeout_for_env(&env, Duration::from_secs(10), Duration::from_secs(45)),
        Duration::from_secs(10)
    );
}

#[test]
fn probe_timeout_for_env_uses_container_timeout_when_container_exec_is_present() {
    let mut env = HashMap::<String, String>::new();
    env.insert(
        "CTX_HARNESS_CONTAINER_ID".to_string(),
        "ctx-workspace-123".to_string(),
    );
    assert_eq!(
        probe_timeout_for_env(&env, Duration::from_secs(10), Duration::from_secs(45)),
        Duration::from_secs(45)
    );
}

#[test]
fn build_crp_session_config_sets_pragmatic_personality_for_codex() {
    let mut env = HashMap::new();
    disable_ctx_mcp(&mut env);
    env.insert("CTX_PROVIDER_ID".to_string(), "codex".to_string());
    let workdir = PathBuf::from("/tmp/workdir");

    let cfg = build_crp_session_config(&env, &workdir).expect("build session config");
    assert_eq!(cfg.approval_policy, None);
    assert_eq!(cfg.sandbox_mode, None);
    assert_eq!(cfg.reasoning_trace_enabled, Some(true));
    assert_eq!(cfg.personality.as_deref(), Some("pragmatic"));
}

#[test]
fn build_crp_session_config_omits_personality_for_non_codex() {
    let mut env = HashMap::new();
    disable_ctx_mcp(&mut env);
    env.insert("CTX_PROVIDER_ID".to_string(), "claude-crp".to_string());
    let workdir = PathBuf::from("/tmp/workdir");

    let cfg = build_crp_session_config(&env, &workdir).expect("build session config");
    assert_eq!(cfg.approval_policy, None);
    assert_eq!(cfg.sandbox_mode, None);
    assert_eq!(cfg.personality, None);
}

#[test]
fn build_crp_session_config_uses_explicit_full_launch_policy_env() {
    let mut env = HashMap::new();
    disable_ctx_mcp(&mut env);
    env.insert(
        CTX_CRP_LAUNCH_POLICY_ENV.to_string(),
        CTX_CRP_LAUNCH_POLICY_FULL.to_string(),
    );
    let workdir = PathBuf::from("/tmp/workdir");

    let cfg = build_crp_session_config(&env, &workdir).expect("build session config");
    assert_eq!(
        cfg.approval_policy.as_deref(),
        Some(FULL_YOLO_APPROVAL_POLICY)
    );
    assert_eq!(cfg.sandbox_mode.as_deref(), Some(FULL_YOLO_SANDBOX_MODE));
}

#[test]
fn build_crp_session_config_rejects_unsupported_launch_policy_env() {
    let mut env = HashMap::new();
    disable_ctx_mcp(&mut env);
    env.insert(
        CTX_CRP_LAUNCH_POLICY_ENV.to_string(),
        "danger-full-access".to_string(),
    );

    let err = build_crp_session_config(&env, Path::new("/tmp/workdir"))
        .expect_err("unsupported launch policy should fail closed");
    assert!(err
        .to_string()
        .contains("unsupported CTX_CRP_LAUNCH_POLICY"));
}

#[test]
fn build_crp_session_config_can_disable_model_override() {
    let mut env = HashMap::new();
    disable_ctx_mcp(&mut env);
    env.insert(
        "CTX_MODEL_ID".to_string(),
        "openai/gpt-4.1-mini".to_string(),
    );
    env.insert(
        "CTX_CRP_DISABLE_MODEL_OVERRIDE".to_string(),
        "1".to_string(),
    );
    let workdir = PathBuf::from("/tmp/workdir");

    let cfg = build_crp_session_config(&env, &workdir).expect("build session config");
    assert_eq!(cfg.model, None);
    assert_eq!(cfg.reasoning_effort, None);
}

#[test]
fn build_crp_session_config_sets_model_provider_from_env() {
    let mut env = HashMap::new();
    disable_ctx_mcp(&mut env);
    env.insert("CTX_MODEL_PROVIDER".to_string(), " openrouter ".to_string());
    let workdir = PathBuf::from("/tmp/workdir");

    let cfg = build_crp_session_config(&env, &workdir).expect("build session config");
    assert_eq!(cfg.model_provider.as_deref(), Some("openrouter"));
}

#[test]
fn build_crp_model_probe_config_sets_model_provider_from_env() {
    let mut env = HashMap::new();
    env.insert("CTX_MODEL_PROVIDER".to_string(), "openrouter".to_string());
    let workdir = PathBuf::from("/tmp/workdir");

    let cfg = build_crp_model_probe_config(&env, &workdir).expect("build model probe config");
    assert_eq!(cfg.model_provider.as_deref(), Some("openrouter"));
}

#[test]
fn build_crp_session_config_sets_openai_base_url_from_env() {
    let mut env = HashMap::new();
    disable_ctx_mcp(&mut env);
    env.insert(
        "OPENAI_BASE_URL".to_string(),
        " https://openrouter.ai/api/v1 ".to_string(),
    );
    let workdir = PathBuf::from("/tmp/workdir");

    let cfg = build_crp_session_config(&env, &workdir).expect("build session config");
    assert_eq!(
        cfg.openai_base_url.as_deref(),
        Some("https://openrouter.ai/api/v1")
    );
}

#[test]
fn build_crp_session_config_scopes_auth_tokens_to_ctx_mcp_server() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let mut env = HashMap::new();
    env.insert(
        "CTX_DAEMON_URL".to_string(),
        "https://daemon.internal".to_string(),
    );
    env.insert(
        "CTX_MCP_COMMAND".to_string(),
        write_test_ctx_mcp_command(tempdir.path()),
    );
    env.insert("CTX_MCP_TOKEN".to_string(), "mcp-token".to_string());
    env.insert("CTX_SESSION_ID".to_string(), "session-123".to_string());
    env.insert("CTX_WORKTREE_ID".to_string(), "worktree-123".to_string());
    let workdir = PathBuf::from("/tmp/workdir");

    let cfg = build_crp_session_config(&env, &workdir).expect("build session config");
    let mcp_env = cfg
        .mcp_servers
        .as_ref()
        .and_then(|servers| servers.get("ctx"))
        .and_then(|server| server.env.as_ref())
        .expect("ctx mcp env");
    assert_eq!(
        mcp_env.get("CTX_DAEMON_URL").map(String::as_str),
        Some("https://daemon.internal")
    );
    assert_eq!(
        mcp_env.get("CTX_MCP_TOKEN").map(String::as_str),
        Some("mcp-token")
    );
    assert!(
        !mcp_env.contains_key("CTX_AUTH_TOKEN"),
        "ctx-mcp env should not receive the daemon bearer"
    );
    assert!(
        !mcp_env.contains_key("CTX_SESSION_ID"),
        "ctx-mcp env should derive session scope from the daemon token"
    );
    assert!(
        !mcp_env.contains_key("CTX_WORKTREE_ID"),
        "ctx-mcp env should derive worktree scope from the daemon token"
    );
}

#[test]
fn build_crp_model_probe_config_sets_openai_base_url_from_env() {
    let mut env = HashMap::new();
    env.insert(
        "OPENAI_BASE_URL".to_string(),
        "https://openrouter.ai/api/v1".to_string(),
    );
    let workdir = PathBuf::from("/tmp/workdir");

    let cfg = build_crp_model_probe_config(&env, &workdir).expect("build model probe config");
    assert_eq!(
        cfg.openai_base_url.as_deref(),
        Some("https://openrouter.ai/api/v1")
    );
}

#[test]
fn build_crp_session_config_rejects_missing_ctx_mcp_command_for_container() {
    let mut env = HashMap::new();
    env.insert(
        "CTX_HARNESS_CONTAINER_ID".to_string(),
        "ctx-harness-123".to_string(),
    );
    env.insert(
        "CTX_MCP_COMMAND".to_string(),
        "/definitely/missing/ctx-mcp".to_string(),
    );

    let err = build_crp_session_config(&env, Path::new("/ctx/ws"))
        .expect_err("missing command should fail closed");
    assert!(err.to_string().contains("path does not exist"));
}

#[test]
fn build_crp_session_config_rejects_bare_ctx_mcp_command() {
    let mut env = HashMap::new();
    env.insert("CTX_MCP_COMMAND".to_string(), "ctx-mcp".to_string());

    let err = build_crp_session_config(&env, Path::new("/tmp/workdir"))
        .expect_err("bare command should fail closed");
    assert!(err
        .to_string()
        .contains("must be an explicit absolute path"));
}

#[test]
fn build_crp_session_config_requires_ctx_mcp_command_when_enabled() {
    let env = HashMap::new();

    let err = build_crp_session_config(&env, Path::new("/tmp/workdir"))
        .expect_err("missing command should fail closed");
    assert!(err.to_string().contains("CTX_MCP_COMMAND is required"));
}

#[test]
fn build_crp_session_config_preserves_existing_absolute_ctx_mcp_for_container() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let mcp_path = tempdir.path().join("ctx-mcp");
    fs::write(&mcp_path, b"#!/bin/sh\n").expect("write mcp");

    let mut env = HashMap::new();
    env.insert(
        "CTX_HARNESS_CONTAINER_ID".to_string(),
        "ctx-harness-123".to_string(),
    );
    env.insert(
        "CTX_MCP_COMMAND".to_string(),
        mcp_path.to_string_lossy().to_string(),
    );

    let cfg = build_crp_session_config(&env, Path::new("/ctx/ws")).expect("build session config");
    let command = cfg
        .mcp_servers
        .as_ref()
        .and_then(|servers| servers.get("ctx"))
        .and_then(|server| server.command.as_deref());
    assert_eq!(command, Some(mcp_path.to_string_lossy().as_ref()));
}

#[test]
fn build_crp_session_config_rewrites_shared_vm_ctx_mcp_command_for_container() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let data_root = tempdir.path().join("data-root");
    let worktree = tempdir.path().join("repo");
    fs::create_dir_all(&worktree).expect("mkdir worktree");
    let mcp_path = data_root.join("bundles/runtimes/ctx-mcp/macos/aarch64/ctx-mcp");
    let linux_mcp_path = data_root.join("bundles/runtimes/ctx-mcp/linux/aarch64/ctx-mcp");
    fs::create_dir_all(mcp_path.parent().expect("parent")).expect("mkdir mcp parent");
    fs::create_dir_all(linux_mcp_path.parent().expect("parent")).expect("mkdir linux mcp parent");
    fs::write(&mcp_path, b"#!/bin/sh\n").expect("write mcp");
    fs::write(&linux_mcp_path, b"#!/bin/sh\n").expect("write linux mcp");

    let mut env = HashMap::new();
    env.insert(
        "CTX_HARNESS_RUNTIME_KIND".to_string(),
        "shared_vm_container".to_string(),
    );
    env.insert(
        "CTX_AVF_LINUX_HELPER_PATH".to_string(),
        "/tmp/ctx-avf-linux-helper".to_string(),
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
        worktree.to_string_lossy().to_string(),
    );
    env.insert(
        "CTX_AVF_GUEST_WORKTREE_ROOT".to_string(),
        "/ctx/ws/worktrees/worktree-1".to_string(),
    );
    env.insert(
        "CTX_HARNESS_GUEST_WORKSPACE_ROOT".to_string(),
        "/ctx/ws".to_string(),
    );
    env.insert(
        "CTX_MCP_COMMAND".to_string(),
        mcp_path.to_string_lossy().to_string(),
    );

    let cfg = build_crp_session_config(&env, &worktree).expect("build session config");
    let command = cfg
        .mcp_servers
        .as_ref()
        .and_then(|servers| servers.get("ctx"))
        .and_then(|server| server.command.as_deref());
    assert_eq!(command, Some(linux_mcp_path.to_string_lossy().as_ref()));
}

#[test]
fn build_crp_model_probe_config_omits_launch_policy_when_unset() {
    let workdir = PathBuf::from("/tmp/workdir");

    let cfg =
        build_crp_model_probe_config(&HashMap::new(), &workdir).expect("build model probe config");
    assert_eq!(cfg.approval_policy, None);
    assert_eq!(cfg.sandbox_mode, None);
}

#[test]
fn build_crp_model_probe_config_uses_explicit_full_launch_policy_env() {
    let mut env = HashMap::new();
    env.insert(
        CTX_CRP_LAUNCH_POLICY_ENV.to_string(),
        CTX_CRP_LAUNCH_POLICY_FULL.to_string(),
    );
    let workdir = PathBuf::from("/tmp/workdir");

    let cfg = build_crp_model_probe_config(&env, &workdir).expect("build model probe config");
    assert_eq!(
        cfg.approval_policy.as_deref(),
        Some(FULL_YOLO_APPROVAL_POLICY)
    );
    assert_eq!(cfg.sandbox_mode.as_deref(), Some(FULL_YOLO_SANDBOX_MODE));
}

#[test]
fn build_crp_session_config_maps_container_thread_cwd_to_guest_worktree() {
    let mut env = HashMap::new();
    disable_ctx_mcp(&mut env);
    env.insert(
        "CTX_HARNESS_CONTAINER_ID".to_string(),
        "ctx-harness-123".to_string(),
    );
    env.insert(
        "CTX_HARNESS_HOST_WORKTREE_ROOT".to_string(),
        "/home/fixture/code/repo".to_string(),
    );
    env.insert(
        "CTX_HARNESS_GUEST_WORKTREE_ROOT".to_string(),
        "/ctx/ws/worktrees/wt-123".to_string(),
    );
    env.insert(
        "CTX_HARNESS_GUEST_WORKSPACE_ROOT".to_string(),
        "/ctx/ws".to_string(),
    );
    let workdir = PathBuf::from("/home/fixture/code/repo/src");

    let cfg = build_crp_session_config(&env, &workdir).expect("build session config");
    assert_eq!(cfg.cwd, Some(PathBuf::from("/ctx/ws/worktrees/wt-123/src")));
    assert_eq!(
        cfg.spawn_cwd,
        Some(PathBuf::from("/ctx/ws/worktrees/wt-123/src"))
    );
}

#[test]
fn build_crp_model_probe_config_maps_container_spawn_cwd_to_guest_worktree() {
    let mut env = HashMap::new();
    env.insert(
        "CTX_HARNESS_CONTAINER_ID".to_string(),
        "ctx-harness-123".to_string(),
    );
    env.insert(
        "CTX_HARNESS_HOST_WORKTREE_ROOT".to_string(),
        "/home/fixture/code/repo".to_string(),
    );
    env.insert(
        "CTX_HARNESS_GUEST_WORKTREE_ROOT".to_string(),
        "/ctx/ws/worktrees/wt-123".to_string(),
    );
    env.insert(
        "CTX_HARNESS_GUEST_WORKSPACE_ROOT".to_string(),
        "/ctx/ws".to_string(),
    );
    let workdir = PathBuf::from("/home/fixture/code/repo/src");

    let cfg = build_crp_model_probe_config(&env, &workdir).expect("build model probe config");
    assert_eq!(cfg.cwd, Some(PathBuf::from("/ctx/ws/worktrees/wt-123/src")));
    assert_eq!(
        cfg.spawn_cwd,
        Some(PathBuf::from("/ctx/ws/worktrees/wt-123/src"))
    );
}

#[test]
fn build_crp_auth_session_config_omits_mcp_servers_but_preserves_other_fields() {
    let mut env = HashMap::new();
    env.insert("CTX_MODEL_ID".to_string(), "openai/gpt-5.5".to_string());
    env.insert("CTX_MODEL_PROVIDER".to_string(), "openrouter".to_string());
    env.insert(
        "OPENAI_BASE_URL".to_string(),
        "https://openrouter.ai/api/v1".to_string(),
    );
    env.insert(
        "CTX_MCP_COMMAND".to_string(),
        "/does/not/matter".to_string(),
    );
    let workdir = PathBuf::from("/tmp/workdir");

    let cfg = build_crp_auth_session_config(&env, &workdir).expect("build auth config");
    assert!(cfg.mcp_servers.is_none());
    assert_eq!(cfg.approval_policy, None);
    assert_eq!(cfg.sandbox_mode, None);
    assert_eq!(cfg.model.as_deref(), Some("openai/gpt-5.5"));
    assert_eq!(cfg.model_provider.as_deref(), Some("openrouter"));
    assert_eq!(
        cfg.openai_base_url.as_deref(),
        Some("https://openrouter.ai/api/v1")
    );
    assert_eq!(cfg.cwd, Some(workdir.clone()));
    assert_eq!(cfg.spawn_cwd, Some(workdir));
}

#[test]
fn synthetic_cline_models_probe_uses_openai_model() {
    let mut env = HashMap::new();
    env.insert(
        "OPENAI_MODEL".to_string(),
        "openai/gpt-5.2-codex".to_string(),
    );
    let probe = synthetic_models_probe_for_provider("cline", &env).expect("cline synthetic probe");
    assert_eq!(
        probe.current_model_id.as_deref(),
        Some("openai/gpt-5.2-codex")
    );
    assert_eq!(probe.models.len(), 1);
    assert_eq!(probe.models[0].id, "openai/gpt-5.2-codex");
}

#[test]
fn synthetic_models_probe_is_provider_scoped() {
    let env = HashMap::new();
    assert!(synthetic_models_probe_for_provider("qwen", &env).is_none());
}

#[test]
fn synthetic_cline_models_probe_requires_openai_model() {
    let env = HashMap::new();
    assert!(synthetic_models_probe_for_provider("cline", &env).is_none());
}

#[test]
fn flatten_prompt_items_as_text_joins_text_blocks_in_order() {
    let items = vec![
        json!({"type":"text","text":"system"}),
        json!({"type":"text","text":"user"}),
    ];
    assert_eq!(
        flatten_prompt_items_as_text(&items).expect("flattened prompt"),
        "system\n\nuser"
    );
}

#[test]
fn flatten_prompt_items_as_text_rejects_non_text_items() {
    let items = vec![json!({"type":"image","image_url":"data:image/png;base64,AAAA"})];
    let err = flatten_prompt_items_as_text(&items).expect_err("image item should fail");
    assert!(err
        .to_string()
        .contains("provider requires text-only ACP prompt items"));
}

#[tokio::test]
async fn build_prompt_items_emits_inline_images_as_bytes_items() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut env = HashMap::new();
    env.insert(
        "CTX_DATA_ROOT".to_string(),
        temp.path().to_string_lossy().to_string(),
    );
    let bytes = vec![1u8, 2, 3, 4];
    let input = TurnInput {
        content: "describe image".to_string(),
        context_blocks: Vec::new(),
        attachments: vec![ctx_core::models::MessageAttachment::Image {
            mime_type: "image/png".to_string(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
            name: Some("inline.png".to_string()),
        }],
        model_id: None,
    };

    let items = build_prompt_items(&input, &PathBuf::from("."), &env)
        .await
        .expect("inline image should be emitted as CRP image item");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].get("type").and_then(Value::as_str), Some("image"));
    assert_eq!(
        items[0].get("mime_type").and_then(Value::as_str),
        Some("image/png")
    );
    assert_eq!(
        items[0].get("data").and_then(Value::as_str),
        Some(
            base64::engine::general_purpose::STANDARD
                .encode(&bytes)
                .as_str()
        )
    );
}

#[tokio::test]
async fn build_prompt_items_emits_blob_refs_as_image_refs() {
    let host_root = tempfile::tempdir().expect("host tempdir");
    let runtime_root = tempfile::tempdir().expect("runtime tempdir");
    let blob_dir = host_root.path().join("blobs");
    tokio::fs::create_dir_all(&blob_dir)
        .await
        .expect("create blob dir");
    let blob_id = "blob-123";
    let blob_path = blob_dir.join(blob_id);
    let bytes = vec![9u8, 8, 7, 6];
    tokio::fs::write(&blob_path, &bytes)
        .await
        .expect("write blob");

    let mut env = HashMap::new();
    env.insert(
        "CTX_DATA_ROOT_HOST".to_string(),
        host_root.path().to_string_lossy().to_string(),
    );
    env.insert(
        "CTX_DATA_ROOT".to_string(),
        runtime_root.path().to_string_lossy().to_string(),
    );

    let input = TurnInput {
        content: "describe image".to_string(),
        context_blocks: Vec::new(),
        attachments: vec![ctx_core::models::MessageAttachment::ImageRef {
            blob_id: blob_id.to_string(),
            mime_type: "image/png".to_string(),
            name: Some("blob.png".to_string()),
        }],
        model_id: None,
    };

    let items = build_prompt_items(&input, &PathBuf::from("."), &env)
        .await
        .expect("blob image should be emitted as image_ref");
    assert_eq!(items.len(), 2);
    assert_eq!(
        items[0].get("type").and_then(Value::as_str),
        Some("image_ref")
    );
    assert_eq!(
        items[0].get("blob_id").and_then(Value::as_str),
        Some(blob_id)
    );
    assert_eq!(
        items[0].get("mime_type").and_then(Value::as_str),
        Some("image/png")
    );
}
