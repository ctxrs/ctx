use std::collections::HashMap;

#[cfg(test)]
use ctx_core::ids::SessionId;
use ctx_providers::ask_user_question::AskUserQuestionOutcome;

#[derive(Debug)]
pub struct SubmitAskUserAnswer {
    pub tool_call_id: String,
    pub outcome: AskUserQuestionOutcome,
    pub answers: HashMap<String, String>,
}

#[derive(Debug)]
pub enum SubmitAskUserAnswerError {
    MissingToolCallId,
    SessionNotFound,
    StoreUnavailable(anyhow::Error),
    LoadSession,
    NoPendingQuestion,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::daemon::{route_handles_from_state, DaemonState};
    use ctx_core::models::ExecutionEnvironment;
    use ctx_store::StoreManager;

    async fn test_state(root: &std::path::Path) -> Arc<DaemonState> {
        Arc::new(DaemonState::new(
            root.to_path_buf(),
            StoreManager::open(root).await.unwrap(),
            HashMap::new(),
            "http://127.0.0.1:4399".to_string(),
            Some("daemon-secret".to_string()),
        ))
    }

    async fn create_session(state: &Arc<DaemonState>, root: &std::path::Path) -> SessionId {
        let workspace = state
            .global_store()
            .create_workspace(
                "ask-user".to_string(),
                root.join("workspace").to_string_lossy().to_string(),
                ctx_core::models::VcsKind::Git,
            )
            .await
            .unwrap();
        let store = state.store_for_workspace(workspace.id).await.unwrap();
        let worktree = store
            .create_worktree(
                workspace.id,
                root.join("worktree").to_string_lossy().to_string(),
                "deadbeef".to_string(),
                None,
            )
            .await
            .unwrap();
        state
            .global_store()
            .upsert_workspace_worktree_index(worktree.id, workspace.id)
            .await
            .unwrap();
        let task = store
            .create_task(workspace.id, "task".to_string(), None)
            .await
            .unwrap();
        let session = store
            .create_session(
                task.id,
                workspace.id,
                worktree.id,
                ExecutionEnvironment::Host,
                "fake".to_string(),
                "model".to_string(),
                "implementer".to_string(),
                None,
                None,
                None,
            )
            .await
            .unwrap();
        state
            .global_store()
            .upsert_workspace_session_index(session.id, workspace.id)
            .await
            .unwrap();
        session.id
    }

    async fn submit_ask_user_answer_for_test(
        state: &Arc<DaemonState>,
        session_id: SessionId,
        submission: SubmitAskUserAnswer,
    ) -> Result<(), SubmitAskUserAnswerError> {
        route_handles_from_state(state)
            .session_control
            .submit_ask_user_answer(session_id, submission)
            .await
    }

    async fn make_workspace_store_unopenable_for_session(
        root: &std::path::Path,
        state: &Arc<DaemonState>,
        session_id: SessionId,
    ) {
        let store = state.store_for_session(session_id).await.unwrap();
        let session = store.get_session(session_id).await.unwrap().unwrap();
        state.task_session_cleanup.cleanup_session(session.id).await;
        state
            .core
            .stores
            .evict_workspace(session.workspace_id)
            .await;

        let workspace_store_path = root
            .join("db")
            .join("workspaces")
            .join(session.workspace_id.0.to_string());
        match tokio::fs::metadata(&workspace_store_path).await {
            Ok(metadata) if metadata.is_dir() => tokio::fs::remove_dir_all(&workspace_store_path)
                .await
                .expect("remove workspace store dir"),
            Ok(_) => tokio::fs::remove_file(&workspace_store_path)
                .await
                .expect("remove workspace store file"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("stat workspace store path: {error:#}"),
        }
        tokio::fs::create_dir_all(
            workspace_store_path
                .parent()
                .expect("workspace store parent"),
        )
        .await
        .expect("create workspace store parent");
        tokio::fs::write(&workspace_store_path, b"blocked workspace store")
            .await
            .expect("block workspace store");
    }

    #[tokio::test]
    async fn submit_ask_user_answer_fulfills_pending_question_and_records_notice() {
        let root = tempfile::tempdir().unwrap();
        let state = test_state(root.path()).await;
        let session_id = create_session(&state, root.path()).await;
        let receiver = state
            .core
            .ask_user_question
            .begin(session_id.0.to_string(), "tool-1".to_string())
            .await;

        let mut answers = HashMap::new();
        answers.insert("choice".to_string(), "ship".to_string());
        submit_ask_user_answer_for_test(
            &state,
            session_id,
            SubmitAskUserAnswer {
                tool_call_id: " tool-1 ".to_string(),
                outcome: AskUserQuestionOutcome::Submitted,
                answers: answers.clone(),
            },
        )
        .await
        .unwrap();

        let answer = receiver.await.unwrap();
        assert_eq!(answer.outcome, AskUserQuestionOutcome::Submitted);
        assert_eq!(answer.answers, answers);

        let store = state.store_for_session(session_id).await.unwrap();
        let events = store.list_session_events(session_id).await.unwrap();
        assert!(events.iter().any(|event| {
            event.payload_json.get("kind").and_then(|v| v.as_str())
                == Some("ask_user_question_answered")
                && event
                    .payload_json
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    == Some("tool-1")
        }));
    }

    #[tokio::test]
    async fn submit_ask_user_answer_rejects_missing_tool_call_id() {
        let root = tempfile::tempdir().unwrap();
        let state = test_state(root.path()).await;
        let session_id = create_session(&state, root.path()).await;

        let error = submit_ask_user_answer_for_test(
            &state,
            session_id,
            SubmitAskUserAnswer {
                tool_call_id: "   ".to_string(),
                outcome: AskUserQuestionOutcome::Submitted,
                answers: HashMap::new(),
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(error, SubmitAskUserAnswerError::MissingToolCallId));
    }

    #[tokio::test]
    async fn submit_ask_user_answer_rejects_without_pending_question() {
        let root = tempfile::tempdir().unwrap();
        let state = test_state(root.path()).await;
        let session_id = create_session(&state, root.path()).await;

        let error = submit_ask_user_answer_for_test(
            &state,
            session_id,
            SubmitAskUserAnswer {
                tool_call_id: "tool-1".to_string(),
                outcome: AskUserQuestionOutcome::Submitted,
                answers: HashMap::new(),
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(error, SubmitAskUserAnswerError::NoPendingQuestion));
    }

    #[tokio::test]
    async fn submit_ask_user_answer_rejects_missing_session() {
        let root = tempfile::tempdir().unwrap();
        let state = test_state(root.path()).await;

        let error = submit_ask_user_answer_for_test(
            &state,
            SessionId::new(),
            SubmitAskUserAnswer {
                tool_call_id: "tool-1".to_string(),
                outcome: AskUserQuestionOutcome::Submitted,
                answers: HashMap::new(),
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(error, SubmitAskUserAnswerError::SessionNotFound));
    }

    #[tokio::test]
    async fn submit_ask_user_answer_rejects_deleting_workspace_as_missing_session() {
        let root = tempfile::tempdir().unwrap();
        let state = test_state(root.path()).await;
        let session_id = create_session(&state, root.path()).await;
        let store = state.store_for_session(session_id).await.unwrap();
        let session = store.get_session(session_id).await.unwrap().unwrap();
        state
            .core
            .stores
            .begin_workspace_delete(session.workspace_id)
            .await;

        let error = submit_ask_user_answer_for_test(
            &state,
            session_id,
            SubmitAskUserAnswer {
                tool_call_id: "tool-1".to_string(),
                outcome: AskUserQuestionOutcome::Submitted,
                answers: HashMap::new(),
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(error, SubmitAskUserAnswerError::SessionNotFound));
        state
            .core
            .stores
            .finish_workspace_delete(session.workspace_id)
            .await;
    }

    #[tokio::test]
    async fn submit_ask_user_answer_rejects_unavailable_workspace_store() {
        let root = tempfile::tempdir().unwrap();
        let state = test_state(root.path()).await;
        let session_id = create_session(&state, root.path()).await;
        make_workspace_store_unopenable_for_session(root.path(), &state, session_id).await;

        let error = submit_ask_user_answer_for_test(
            &state,
            session_id,
            SubmitAskUserAnswer {
                tool_call_id: "tool-1".to_string(),
                outcome: AskUserQuestionOutcome::Submitted,
                answers: HashMap::new(),
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            SubmitAskUserAnswerError::StoreUnavailable(_)
        ));
    }

    #[tokio::test]
    async fn submit_ask_user_answer_rejects_archived_subagent_session() {
        let root = tempfile::tempdir().unwrap();
        let state = test_state(root.path()).await;
        let parent_id = create_session(&state, root.path()).await;
        let store = state.store_for_session(parent_id).await.unwrap();
        let parent = store.get_session(parent_id).await.unwrap().unwrap();
        let task = store
            .create_task(parent.workspace_id, "child-task".to_string(), None)
            .await
            .unwrap();
        let child = store
            .create_session(
                task.id,
                parent.workspace_id,
                parent.worktree_id,
                ExecutionEnvironment::Host,
                "fake".to_string(),
                "model".to_string(),
                "implementer".to_string(),
                Some(parent.id),
                Some("sub_agent".to_string()),
                None,
            )
            .await
            .unwrap();
        state
            .global_store()
            .upsert_workspace_session_index(child.id, child.workspace_id)
            .await
            .unwrap();
        assert!(store
            .archive_subagent_session(parent.id, child.id)
            .await
            .unwrap());

        let error = submit_ask_user_answer_for_test(
            &state,
            child.id,
            SubmitAskUserAnswer {
                tool_call_id: "tool-1".to_string(),
                outcome: AskUserQuestionOutcome::Submitted,
                answers: HashMap::new(),
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(error, SubmitAskUserAnswerError::SessionNotFound));
    }
}
