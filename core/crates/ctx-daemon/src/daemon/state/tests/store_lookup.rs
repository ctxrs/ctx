use super::*;

use ctx_core::models::{ExecutionEnvironment, Session, VcsKind};

async fn seeded_session() -> (tempfile::TempDir, Arc<DaemonState>, Session) {
    let temp = tempfile::tempdir().expect("create tempdir");
    let state = fixtures::test_state(&temp).await;

    let workspace = state
        .global_store()
        .create_workspace(
            "ws".to_string(),
            temp.path().to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .expect("create workspace");
    let store = state
        .store_for_workspace(workspace.id)
        .await
        .expect("open workspace store");
    let worktree = store
        .create_worktree(
            workspace.id,
            temp.path().to_string_lossy().to_string(),
            "base".to_string(),
            None,
        )
        .await
        .expect("create worktree");
    state
        .global_store()
        .upsert_workspace_worktree_index(worktree.id, workspace.id)
        .await
        .expect("index worktree");
    let task = store
        .create_task(workspace.id, "task".to_string(), None)
        .await
        .expect("create task");
    state
        .global_store()
        .upsert_workspace_task_index(task.id, workspace.id)
        .await
        .expect("index task");
    let session = store
        .create_session(
            task.id,
            workspace.id,
            worktree.id,
            ExecutionEnvironment::Host,
            "fake".to_string(),
            "fake-model".to_string(),
            "implementer".to_string(),
            None,
            None,
            None,
        )
        .await
        .expect("create session");
    state
        .global_store()
        .upsert_workspace_session_index(session.id, workspace.id)
        .await
        .expect("index session");

    (temp, state, session)
}

async fn block_workspace_store_for_session(
    data_dir: &tempfile::TempDir,
    state: &Arc<DaemonState>,
    session: &Session,
) {
    state.task_session_cleanup.cleanup_session(session.id).await;
    state
        .core
        .stores
        .evict_workspace(session.workspace_id)
        .await;

    let blocked_workspace_store_dir = data_dir
        .path()
        .join("db")
        .join("workspaces")
        .join(session.workspace_id.0.to_string());
    if let Ok(metadata) = tokio::fs::metadata(&blocked_workspace_store_dir).await {
        if metadata.is_dir() {
            tokio::fs::remove_dir_all(&blocked_workspace_store_dir)
                .await
                .expect("remove workspace store dir");
        } else {
            tokio::fs::remove_file(&blocked_workspace_store_dir)
                .await
                .expect("remove workspace store file");
        }
    }
    tokio::fs::create_dir_all(
        blocked_workspace_store_dir
            .parent()
            .expect("workspace store parent"),
    )
    .await
    .expect("create workspace store parent");
    tokio::fs::write(&blocked_workspace_store_dir, b"blocked workspace store")
        .await
        .expect("block workspace store");
}

async fn session_store_error(
    result: Result<ctx_store::Store, SessionStoreAccessError>,
    message: &str,
) -> SessionStoreAccessError {
    match result {
        Ok(_) => panic!("{message}"),
        Err(error) => error,
    }
}

async fn workspace_store_error(
    result: Result<ctx_store::Store, WorkspaceStoreAccessError>,
    message: &str,
) -> WorkspaceStoreAccessError {
    match result {
        Ok(_) => panic!("{message}"),
        Err(error) => error,
    }
}

#[tokio::test]
async fn existing_session_store_reports_lookup_unavailable_when_workspace_store_cannot_open() {
    let (data_dir, state, session) = seeded_session().await;
    block_workspace_store_for_session(&data_dir, &state, &session).await;

    let error = session_store_error(
        state.existing_session_store(session.id).await,
        "blocked workspace store should fail reads",
    )
    .await;

    assert!(
        matches!(error, SessionStoreAccessError::LookupUnavailable(_)),
        "unexpected error: {error:?}"
    );
}

#[tokio::test]
async fn existing_session_store_for_write_retries_and_reports_store_unavailable() {
    let (data_dir, state, session) = seeded_session().await;
    block_workspace_store_for_session(&data_dir, &state, &session).await;

    let error = session_store_error(
        state.existing_session_store_for_write(session.id).await,
        "blocked workspace store should fail writes",
    )
    .await;

    assert!(
        matches!(error, SessionStoreAccessError::StoreUnavailable),
        "unexpected error: {error:?}"
    );
}

#[tokio::test]
async fn existing_session_store_treats_deleting_workspace_as_not_found() {
    let (_data_dir, state, session) = seeded_session().await;
    state
        .core
        .stores
        .begin_workspace_delete(session.workspace_id)
        .await;

    let error = session_store_error(
        state.existing_session_store(session.id).await,
        "deleting workspace should hide session",
    )
    .await;
    assert!(
        matches!(error, SessionStoreAccessError::NotFound),
        "unexpected error: {error:?}"
    );

    state
        .core
        .stores
        .finish_workspace_delete(session.workspace_id)
        .await;
}

#[tokio::test]
async fn existing_session_store_hides_archived_subagents_except_explicit_history_access() {
    let (_data_dir, state, session) = seeded_session().await;
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

    let error = session_store_error(
        state.existing_session_store(child.id).await,
        "generic reads should hide archived subagents",
    )
    .await;
    assert!(
        matches!(error, SessionStoreAccessError::NotFound),
        "unexpected read error: {error:?}"
    );

    let error = session_store_error(
        state.existing_session_store_for_write(child.id).await,
        "writes should hide archived subagents",
    )
    .await;
    assert!(
        matches!(error, SessionStoreAccessError::NotFound),
        "unexpected write error: {error:?}"
    );

    state
        .existing_session_store_allow_archived(child.id)
        .await
        .expect("history access should allow archived subagents");
}

#[tokio::test]
async fn weak_session_store_lookup_stops_after_session_runtime_drops() {
    let (_data_dir, state, session) = seeded_session().await;
    let lookup = WeakSessionStoreLookup::new(
        state.global_store().clone(),
        state.core.stores.clone(),
        Arc::downgrade(&state.sessions),
        Arc::clone(&state.transport.merge_queue),
    );

    assert!(
        lookup
            .existing_session_store_allow_archived(session.id)
            .await
            .expect("live runtime lookup should not error")
            .is_some(),
        "live runtime should permit lookup"
    );

    drop(state);

    assert!(
        lookup
            .existing_session_store_allow_archived(session.id)
            .await
            .expect("dropped runtime lookup should not error")
            .is_none(),
        "dropped runtime should stop weak lookup without retaining the runtime"
    );
}

#[tokio::test]
async fn existing_workspace_store_treats_deleting_workspace_as_not_found() {
    let (_data_dir, state, session) = seeded_session().await;
    state
        .core
        .stores
        .begin_workspace_delete(session.workspace_id)
        .await;

    let error = workspace_store_error(
        state.existing_workspace_store(session.workspace_id).await,
        "deleting workspace should look missing",
    )
    .await;
    assert!(
        matches!(error, WorkspaceStoreAccessError::NotFound),
        "unexpected error: {error:?}"
    );

    state
        .core
        .stores
        .finish_workspace_delete(session.workspace_id)
        .await;
}
