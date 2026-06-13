use super::*;
use ctx_session_service::session_creation::{
    prepare_loaded_session_request as prepare_loaded_session_request_policy,
    LoadedSessionRequestError, LoadedSessionRequestInput,
};
use model::{resolve_loaded_session_model, LoadedSessionModelRequest, ResolvedLoadedSessionModel};

#[path = "prepared/model.rs"]
mod model;

pub(super) struct PreparedLoadedSessionRequest {
    pub(super) run_id_header: Option<String>,
    pub(super) provider_id: String,
    pub(super) session_id: Option<SessionId>,
    pub(super) parent_session_id: Option<SessionId>,
    pub(super) relationship: Option<String>,
    pub(super) requested_relationship: Option<String>,
    pub(super) worktree_id: WorktreeId,
    pub(super) created_worktree_id: Option<WorktreeId>,
    pub(super) execution_environment: ExecutionEnvironment,
    pub(super) model_id: String,
    pub(super) reasoning_effort: Option<String>,
    pub(super) preferred_model_id: String,
}

pub(super) async fn prepare_loaded_session_request(
    handles: &TaskSessionHandles,
    store: &Store,
    task: &Task,
    workspace: &Workspace,
    input: &CreateTaskSessionInput,
) -> Result<PreparedLoadedSessionRequest, TaskSessionCreateError> {
    let provider_id = input.provider_id.trim().to_string();
    let provider_can_create_loaded_session = handles
        .admission
        .can_create_loaded_session_for_provider(&provider_id)
        .await;
    let session_request = match prepare_loaded_session_request_policy(LoadedSessionRequestInput {
        run_id_header: input.run_id_header.as_deref(),
        provider_id: input.provider_id.as_str(),
        provider_can_create_loaded_session,
        requested_session_id: input.id.as_deref(),
        parent_session_id: input.parent_session_id.as_deref(),
        relationship: input.relationship.as_deref(),
        initial_prompt_present: input.initial_prompt.is_some(),
        initial_message_id_present: input.initial_message_id.is_some(),
        initial_turn_id_present: input.initial_turn_id.is_some(),
        task_primary_session_id: task.primary_session_id,
    }) {
        Ok(decision) => decision,
        Err(error) => {
            if let Some(issue) = error.compat_issue() {
                handles
                    .admission
                    .emit_compat_payload_reject_counter("tasks.create_session", issue, None)
                    .await;
            }
            return Err(task_session_create_error_from_loaded_request(error));
        }
    };

    let worktree_resolution = resolve_session_worktree_for_task(
        handles,
        store,
        task,
        workspace,
        input.worktree_id.as_deref(),
        input.execution_environment,
    )
    .await?;
    let worktree_id = worktree_resolution.worktree_id;
    let created_worktree_id = worktree_resolution.created_worktree_id;
    let execution_environment = worktree_resolution.execution_environment;

    let ResolvedLoadedSessionModel {
        model_id,
        reasoning_effort,
        preferred_model_id,
    } = resolve_loaded_session_model(LoadedSessionModelRequest {
        handles,
        store,
        workspace,
        task_id: task.id,
        provider_id: &provider_id,
        execution_environment,
        requested_model_id: input.model_id.as_str(),
        requested_reasoning_effort: input.reasoning_effort.as_deref(),
        created_worktree_id,
    })
    .await?;

    Ok(PreparedLoadedSessionRequest {
        run_id_header: session_request.run_id_header,
        provider_id: session_request.provider_id,
        session_id: session_request.session_id,
        parent_session_id: session_request.parent_session_id,
        relationship: session_request.relationship,
        requested_relationship: session_request.requested_relationship,
        worktree_id,
        created_worktree_id,
        execution_environment,
        model_id,
        reasoning_effort,
        preferred_model_id,
    })
}

fn task_session_create_error_from_loaded_request(
    error: LoadedSessionRequestError,
) -> TaskSessionCreateError {
    match error {
        LoadedSessionRequestError::PrimarySessionConflict => TaskSessionCreateError::Conflict,
        LoadedSessionRequestError::ProviderUnavailable
        | LoadedSessionRequestError::InvalidSessionId
        | LoadedSessionRequestError::InvalidParentSessionId
        | LoadedSessionRequestError::RelationshipRequiresParent
        | LoadedSessionRequestError::MissingInitialPromptIds => TaskSessionCreateError::BadRequest,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDaemon;
    use ctx_core::ids::MessageId;
    use ctx_core::models::VcsKind;
    use ctx_providers::adapters::ProviderAdapter;
    use ctx_providers::fake::FakeProviderAdapter;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[tokio::test]
    async fn missing_initial_ids_emits_create_session_compat_counter() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_root = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace_root).expect("workspace root");
        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("fake".to_string(), Arc::new(FakeProviderAdapter::new()));
        let daemon = TestDaemon::new_with_providers_for_test(
            temp.path().join("data"),
            providers,
            "http://localhost".to_string(),
            None,
        )
        .await
        .expect("daemon");
        let workspace = daemon
            .seed_workspace_for_test("workspace", &workspace_root, VcsKind::Git)
            .await
            .expect("workspace");
        let task = daemon
            .seed_task_default_session_task_for_test(workspace.id, "task")
            .await
            .expect("task");

        let error = daemon
            .task_session_admission_handle_for_test()
            .create_session_for_task(
                task.id,
                CreateTaskSessionInput {
                    id: None,
                    provider_id: "fake".to_string(),
                    model_id: "fake-model".to_string(),
                    reasoning_effort: None,
                    remember_model_preference: false,
                    parent_session_id: None,
                    relationship: None,
                    initial_prompt: Some("hello".to_string()),
                    initial_message_id: Some(MessageId::new().0.to_string()),
                    initial_turn_id: None,
                    worktree_id: None,
                    execution_environment: None,
                    run_id_header: None,
                },
            )
            .await
            .expect_err("missing turn id should be rejected");

        assert!(matches!(error, TaskSessionCreateError::BadRequest));
        let summary = daemon.telemetry_handle_for_test().perf_telemetry().summary(
            Some("compat.payload_reject_count"),
            None,
            None,
            None,
        );
        assert!(summary.metrics.iter().any(|metric| {
            metric.labels.get("source").map(String::as_str) == Some("daemon")
                && metric.labels.get("surface").map(String::as_str) == Some("tasks.create_session")
                && metric.labels.get("issue").map(String::as_str) == Some("missing_initial_ids")
                && metric.sum >= 1.0
        }));
    }
}
