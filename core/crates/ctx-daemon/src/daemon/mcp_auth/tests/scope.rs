use std::collections::HashMap;

use ctx_core::ids::{SessionId, WorkspaceId, WorktreeId};
use ctx_mcp_auth::{McpAuthCapabilities, McpAuthContext};
use ctx_store::StoreManager;

use super::super::{require_scoped_mcp_session_context, ScopedMcpSessionAccessError};
use super::fixtures::{block_workspace_store_for_session, context_for, seeded_state};
use crate::daemon::DaemonState;

#[tokio::test]
async fn scoped_mcp_session_context_accepts_bound_session_scope() {
    let (_data_dir, state, session) = seeded_state().await;

    require_scoped_mcp_session_context(state.as_ref(), context_for(&session), session.id)
        .await
        .expect("matching session scope should be accepted");
}

#[tokio::test]
async fn scoped_mcp_session_context_rejects_wrong_route_session_before_store_lookup() {
    let (_data_dir, state, session) = seeded_state().await;
    let error =
        require_scoped_mcp_session_context(state.as_ref(), context_for(&session), SessionId::new())
            .await
            .expect_err("wrong route session should be unauthorized");

    match error {
        ScopedMcpSessionAccessError::Unauthorized(message) => assert_eq!(
            message,
            "scoped ctx-mcp token is limited to the current session"
        ),
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn scoped_mcp_session_context_rejects_loaded_scope_mismatch() {
    let (_data_dir, state, session) = seeded_state().await;
    let mut context = context_for(&session);
    context.workspace_id = WorkspaceId::new();

    let error = require_scoped_mcp_session_context(state.as_ref(), context, session.id)
        .await
        .expect_err("loaded scope mismatch should be unauthorized");

    match error {
        ScopedMcpSessionAccessError::Unauthorized(message) => assert_eq!(
            message,
            "scoped ctx-mcp token does not match the loaded session scope"
        ),
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn scoped_mcp_session_context_reports_missing_session() {
    let data_dir = tempfile::tempdir().expect("create tempdir");
    let stores = StoreManager::open(data_dir.path())
        .await
        .expect("open stores");
    let state = DaemonState::new(
        data_dir.path().to_path_buf(),
        stores,
        HashMap::new(),
        "http://127.0.0.1:0".to_string(),
        None,
    );
    let session_id = SessionId::new();
    let context = McpAuthContext {
        session_id,
        workspace_id: WorkspaceId::new(),
        worktree_id: WorktreeId::new(),
        capabilities: McpAuthCapabilities::provider_session(),
    };

    let error = require_scoped_mcp_session_context(&state, context, session_id)
        .await
        .expect_err("missing session should be reported as not found");

    assert!(
        matches!(error, ScopedMcpSessionAccessError::SessionNotFound),
        "unexpected error: {error:?}"
    );
}

#[tokio::test]
async fn scoped_mcp_session_context_reports_store_unavailable() {
    let (data_dir, state, session) = seeded_state().await;
    block_workspace_store_for_session(&data_dir, &state, &session).await;

    let error =
        require_scoped_mcp_session_context(state.as_ref(), context_for(&session), session.id)
            .await
            .expect_err("blocked workspace store should report unavailable");

    assert!(
        matches!(error, ScopedMcpSessionAccessError::StoreUnavailable(_)),
        "unexpected error: {error:?}"
    );
}

#[tokio::test]
async fn scoped_mcp_session_context_rejects_archived_subagents() {
    let (_data_dir, state, session) = seeded_state().await;
    let store = state
        .store_for_workspace(session.workspace_id)
        .await
        .expect("open workspace store");
    let child = store
        .create_session(
            session.task_id,
            session.workspace_id,
            session.worktree_id,
            session.execution_environment,
            "fake".to_string(),
            "fake-model".to_string(),
            "subagent".to_string(),
            Some(session.id),
            Some("sub_agent".to_string()),
            None,
        )
        .await
        .expect("create child session");
    state
        .global_store()
        .upsert_workspace_session_index(child.id, session.workspace_id)
        .await
        .expect("index child session");
    assert!(
        store
            .archive_subagent_session(session.id, child.id)
            .await
            .expect("archive subagent session"),
        "child should transition to archived"
    );

    let error = require_scoped_mcp_session_context(state.as_ref(), context_for(&child), child.id)
        .await
        .expect_err("archived subagent should be hidden from MCP scope checks");

    assert!(
        matches!(error, ScopedMcpSessionAccessError::SessionNotFound),
        "unexpected error: {error:?}"
    );
}
