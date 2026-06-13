use super::*;

#[tokio::test]
async fn load_system_prompt_append_for_subagent_combines_agent_and_subagent_append() {
    let temp = tempdir().expect("tempdir");
    let db_path = temp.path().join("workspace.sqlite");
    let store = ctx_store::Store::open_sqlite(&db_path, None)
        .await
        .expect("open workspace store");
    workspace_config::update_agent_system_prompt_append(&store, Some("Agent prompt".to_string()))
        .await
        .expect("save agent prompt append");
    workspace_config::update_subagent_system_prompt_append(
        &store,
        Some("Subagent prompt".to_string()),
    )
    .await
    .expect("save subagent prompt append");

    let append =
        super::helpers::load_system_prompt_append_for_relationship(&store, Some("sub_agent"))
            .await
            .expect("load combined prompt append");
    assert_eq!(append, Some("Agent prompt\n\nSubagent prompt".to_string()));

    store.close().await;
}

#[tokio::test]
async fn load_system_prompt_append_fails_closed_on_invalid_workspace_settings_doc() {
    let temp = tempdir().expect("tempdir");
    let db_path = temp.path().join("workspace.sqlite");
    let store = ctx_store::Store::open_sqlite(&db_path, None)
        .await
        .expect("open workspace store");
    store
        .upsert_runtime_settings_document(1, "{ not valid json")
        .await
        .expect("write invalid workspace settings");

    let err = super::helpers::load_system_prompt_append_for_relationship(&store, None)
        .await
        .expect_err("invalid workspace settings should fail closed");
    let message = format!("{err:#}");
    assert!(
        message.contains("loading agent system prompt append config"),
        "expected loader context in error: {message}"
    );
    assert!(
        message.contains("parsing workspace runtime settings document"),
        "expected parse context in error: {message}"
    );

    store.close().await;
}
