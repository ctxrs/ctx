use super::*;
use tokio::sync::oneshot;
use tokio::time::{timeout, Duration};

#[tokio::test]
async fn update_execution_config_can_leave_runtime_unspecified() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("db.sqlite");
    let store = Store::open_sqlite(&db_path, None)
        .await
        .expect("open sqlite store");

    update_execution_config(
        &store,
        ExecutionConfigUpdate {
            environment: ExecutionEnvironment::Sandbox,
            network_mode: Some(ContainerNetworkMode::LlmOnly),
            allowlist: Some(vec![" api.openai.com ".to_string(), "".to_string()]),
            image: None,
        },
    )
    .await
    .expect("update execution config");

    let loaded = load_execution_settings_override(&store)
        .await
        .expect("load override")
        .expect("execution override");
    assert_eq!(loaded.mode, Some(ExecutionMode::Sandbox));
    assert_eq!(
        loaded.container.network_mode,
        Some(ContainerNetworkMode::LlmOnly)
    );
    assert_eq!(
        loaded.container.allowlist,
        Some(vec!["api.openai.com".to_string()])
    );

    store.close().await;
}

#[test]
fn execution_config_input_normalizes_request_fields() {
    let update = parse_execution_config_update_input(
        "sandbox",
        Some(" allowlist "),
        Some(vec![
            " api.openai.com ".to_string(),
            "".to_string(),
            " db.internal ".to_string(),
        ]),
        true,
    )
    .expect("parse execution update");

    assert_eq!(update.environment, ExecutionEnvironment::Sandbox);
    assert_eq!(update.network_mode, Some(ContainerNetworkMode::Allowlist));
    assert_eq!(
        update.allowlist,
        Some(vec![
            "api.openai.com".to_string(),
            "db.internal".to_string()
        ])
    );

    let override_config = execution_settings_override_from_update(&update);
    assert_eq!(override_config.mode, Some(ExecutionMode::Sandbox));
    assert_eq!(
        override_config.container.network_mode,
        Some(ContainerNetworkMode::Allowlist)
    );
}

#[test]
fn execution_config_input_rejects_unavailable_sandbox_runtime() {
    let error = parse_execution_config_update_input("sandbox", None, None, false)
        .expect_err("sandbox requires runtime availability");

    assert_eq!(error, ExecutionConfigInputError::SandboxRuntimeUnavailable);
    assert!(error.to_string().contains("AVF sandbox is unavailable"));
}

#[test]
fn execution_config_input_rejects_unknown_network_mode() {
    let error = parse_execution_config_update_input("host", Some("public"), None, false)
        .expect_err("unknown network mode should be rejected");

    assert_eq!(error, ExecutionConfigInputError::InvalidNetworkMode);
    assert_eq!(
        error.to_string(),
        "invalid network_mode (expected llm_only|allowlist|all)"
    );
}

#[test]
fn execution_config_projection_preserves_wire_shape() {
    let mut settings = ExecutionSettings {
        mode: ExecutionMode::Sandbox,
        ..ExecutionSettings::default()
    };
    settings.container.network_mode = ContainerNetworkMode::All;
    settings.container.allowlist = vec!["api.openai.com".to_string()];

    let snapshot = project_execution_config("workspace", &settings);

    assert_eq!(snapshot.source, "workspace");
    assert_eq!(snapshot.environment, "sandbox");
    assert_eq!(snapshot.network_mode.as_deref(), Some("all"));
    assert_eq!(snapshot.allowlist, Some(vec!["api.openai.com".to_string()]));
}

#[tokio::test]
async fn merge_queue_config_transition_reports_enabled_state_changes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("db.sqlite");
    let store = Store::open_sqlite(&db_path, None)
        .await
        .expect("open sqlite store");

    let enabled = update_merge_queue_config_with_transition(
        &store,
        MergeQueueConfigUpdate {
            enabled: true,
            target_branch: Some(" dev ".to_string()),
            verify_commands: vec![" pnpm verify ".to_string()],
            push_on_success: Some(true),
            push_remote: Some(" origin ".to_string()),
            push_branch: Some(" dev ".to_string()),
            canonical_sync: Some(MergeQueueCanonicalSync::CleanOnly),
        },
    )
    .await
    .expect("enable merge queue");

    assert_eq!(
        enabled,
        MergeQueueConfigTransition {
            was_enabled: false,
            now_enabled: true,
        }
    );

    let disabled = update_merge_queue_config_with_transition(
        &store,
        MergeQueueConfigUpdate {
            enabled: false,
            target_branch: None,
            verify_commands: Vec::new(),
            push_on_success: None,
            push_remote: None,
            push_branch: None,
            canonical_sync: None,
        },
    )
    .await
    .expect("disable merge queue");

    assert_eq!(
        disabled,
        MergeQueueConfigTransition {
            was_enabled: true,
            now_enabled: false,
        }
    );

    store.close().await;
}

#[tokio::test]
async fn preferred_new_session_model_round_trips_and_clears() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("db.sqlite");
    let store = Store::open_sqlite(&db_path, None)
        .await
        .expect("open sqlite store");

    update_preferred_new_session_model_id(&store, " codex ", Some(" gpt-5.4/xhigh ".to_string()))
        .await
        .expect("persist preferred model");
    update_preferred_new_session_model_id(&store, "claude-crp", Some(" opus/high ".to_string()))
        .await
        .expect("persist second preferred model");

    assert_eq!(
        load_preferred_new_session_model_id(&store, "codex")
            .await
            .expect("load codex pref"),
        Some("gpt-5.4/xhigh".to_string())
    );
    assert_eq!(
        load_preferred_new_session_models(&store)
            .await
            .expect("load pref map"),
        HashMap::from([
            ("claude-crp".to_string(), "opus/high".to_string()),
            ("codex".to_string(), "gpt-5.4/xhigh".to_string()),
        ])
    );

    update_preferred_new_session_model_id(&store, "codex", Some("   ".to_string()))
        .await
        .expect("clear codex pref");
    assert_eq!(
        load_preferred_new_session_model_id(&store, "codex")
            .await
            .expect("load cleared codex pref"),
        None
    );
    assert_eq!(
        load_preferred_new_session_models(&store)
            .await
            .expect("load remaining pref map"),
        HashMap::from([("claude-crp".to_string(), "opus/high".to_string())])
    );

    update_preferred_new_session_model_id(&store, "claude-crp", None)
        .await
        .expect("clear last pref");
    assert_eq!(
        load_preferred_new_session_models(&store)
            .await
            .expect("load empty pref map"),
        HashMap::new()
    );

    store.close().await;
}

#[tokio::test]
async fn preferred_new_session_model_requires_provider_id() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("db.sqlite");
    let store = Store::open_sqlite(&db_path, None)
        .await
        .expect("open sqlite store");

    let error =
        update_preferred_new_session_model_id(&store, "   ", Some("gpt-5.4/xhigh".to_string()))
            .await
            .expect_err("blank provider id should fail");
    assert!(error.to_string().contains("provider_id is required"));

    store.close().await;
}

#[tokio::test]
async fn malformed_preferred_new_session_model_entries_are_ignored() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("db.sqlite");
    let store = Store::open_sqlite(&db_path, None)
        .await
        .expect("open sqlite store");

    store
        .upsert_runtime_settings_document(
            WORKSPACE_SETTINGS_SCHEMA_VERSION,
            r#"{
  "new_session": {
    "preferred_model_by_provider": {
      "codex": 7,
      "claude-crp": " opus/high ",
      "empty": "   "
    }
  }
}"#,
        )
        .await
        .expect("write malformed runtime settings");

    assert_eq!(
        load_preferred_new_session_models(&store)
            .await
            .expect("load preferred model map"),
        HashMap::from([("claude-crp".to_string(), "opus/high".to_string())])
    );
    assert_eq!(
        load_preferred_new_session_model_id(&store, "codex")
            .await
            .expect("load malformed codex preference"),
        None
    );

    store.close().await;
}

#[tokio::test]
async fn prompt_update_helpers_trim_disable_and_clear_agent_and_subagent_prompts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("db.sqlite");
    let store = Store::open_sqlite(&db_path, None)
        .await
        .expect("open sqlite store");

    let agent =
        update_and_load_agent_system_prompt_append(&store, Some("  Agent append  ".to_string()))
            .await
            .expect("update agent prompt");
    assert_eq!(agent.configured_append.as_deref(), Some("Agent append"));
    assert_eq!(agent.effective_append().as_deref(), Some("Agent append"));
    assert_eq!(agent.source(), AgentSystemPromptAppendSource::Config);

    let agent = update_and_load_agent_system_prompt_append(&store, Some("   ".to_string()))
        .await
        .expect("disable agent prompt");
    assert_eq!(agent.configured_append.as_deref(), Some(""));
    assert_eq!(agent.effective_append(), None);
    assert_eq!(agent.source(), AgentSystemPromptAppendSource::Disabled);

    let agent = update_and_load_agent_system_prompt_append(&store, None)
        .await
        .expect("clear agent prompt");
    assert_eq!(agent.configured_append, None);
    assert_eq!(agent.source(), AgentSystemPromptAppendSource::Default);

    let subagent = update_and_load_subagent_system_prompt_append(
        &store,
        Some("  Subagent append  ".to_string()),
    )
    .await
    .expect("update subagent prompt");
    assert_eq!(
        subagent.configured_append.as_deref(),
        Some("Subagent append")
    );
    assert_eq!(
        subagent.effective_append().as_deref(),
        Some("Subagent append")
    );
    assert_eq!(subagent.source(), AgentSystemPromptAppendSource::Config);

    let subagent = update_and_load_subagent_system_prompt_append(&store, None)
        .await
        .expect("clear subagent prompt");
    assert_eq!(subagent.configured_append, None);
    assert_eq!(subagent.source(), AgentSystemPromptAppendSource::Default);

    store.close().await;
}

#[tokio::test]
async fn primary_branch_update_helper_trims_rejects_blank_and_preserves_other_settings() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("db.sqlite");
    let store = Store::open_sqlite(&db_path, None)
        .await
        .expect("open sqlite store");

    assert_eq!(
        load_primary_branch(&store)
            .await
            .expect("load missing branch"),
        None
    );

    update_preferred_new_session_model_id(&store, "codex", Some("gpt-5.4/xhigh".to_string()))
        .await
        .expect("persist unrelated preference");
    let primary_branch = update_and_load_primary_branch(&store, "  dev  ")
        .await
        .expect("update primary branch");

    assert_eq!(primary_branch, "dev");
    assert_eq!(
        load_primary_branch(&store)
            .await
            .expect("load primary branch"),
        Some("dev".to_string())
    );
    assert_eq!(
        load_preferred_new_session_model_id(&store, "codex")
            .await
            .expect("load unrelated preference"),
        Some("gpt-5.4/xhigh".to_string())
    );

    let error = update_and_load_primary_branch(&store, "   ")
        .await
        .expect_err("blank primary branch should fail");
    assert!(error.to_string().contains("primary_branch is required"));

    store.close().await;
}

#[tokio::test]
async fn concurrent_workspace_settings_updates_do_not_clobber_each_other() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("db.sqlite");
    let store = Store::open_sqlite(&db_path, None)
        .await
        .expect("open sqlite store");

    let (loaded_tx, loaded_rx) = oneshot::channel();
    let (resume_tx, resume_rx) = oneshot::channel();
    workspace_settings_test_pause_hook()
        .lock()
        .await
        .insert("primary_branch", (loaded_tx, resume_rx));

    let first_store = store.clone();
    let first = tokio::spawn(async move { update_primary_branch(&first_store, "main").await });
    loaded_rx.await.expect("first update reached pause point");

    let second_store = store.clone();
    let second = tokio::spawn(async move {
        update_preferred_new_session_model_id(
            &second_store,
            "codex",
            Some("gpt-5.4/xhigh".to_string()),
        )
        .await
    });

    let mut second = second;
    let second_blocked = timeout(Duration::from_millis(100), &mut second)
        .await
        .is_err();
    resume_tx.send(()).expect("resume paused update");
    assert!(
        second_blocked,
        "second workspace settings write should wait for the first"
    );

    first
        .await
        .expect("join first")
        .expect("first update succeeds");
    second
        .await
        .expect("join second")
        .expect("second update succeeds");

    assert_eq!(
        load_primary_branch(&store)
            .await
            .expect("load primary branch"),
        Some("main".to_string())
    );
    assert_eq!(
        load_preferred_new_session_model_id(&store, "codex")
            .await
            .expect("load preferred model"),
        Some("gpt-5.4/xhigh".to_string())
    );

    store.close().await;
}
