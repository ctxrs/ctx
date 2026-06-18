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
    use ctx_core::ids::{MessageId, SessionId, TaskId, TurnId, WorktreeId};
    use ctx_core::models::VcsKind;
    use ctx_providers::adapters::ProviderAdapter;
    use ctx_providers::fake::FakeProviderAdapter;
    use std::collections::HashMap;
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;

    fn run_git_for_create_session_test(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_git_repo_for_create_session_test(root: &Path) {
        std::fs::create_dir_all(root).expect("workspace root");
        run_git_for_create_session_test(root, &["init"]);
        run_git_for_create_session_test(root, &["checkout", "-b", "main"]);
        run_git_for_create_session_test(root, &["config", "user.email", "test@example.com"]);
        run_git_for_create_session_test(root, &["config", "user.name", "Test"]);
        std::fs::write(root.join("file.txt"), "hello\n").expect("fixture file");
        run_git_for_create_session_test(root, &["add", "."]);
        run_git_for_create_session_test(root, &["commit", "-m", "init"]);
    }

    fn task_endpoint(task_id: TaskId) -> ContributionEndpoint {
        ContributionEndpoint::Task {
            task_id: Some(task_id),
            id: None,
        }
    }

    fn session_endpoint(session_id: SessionId) -> ContributionEndpoint {
        ContributionEndpoint::Session {
            session_id: Some(session_id),
            provider: None,
            id: None,
            turn_id: None,
            run_id: None,
        }
    }

    fn worktree_endpoint(worktree_id: WorktreeId) -> ContributionEndpoint {
        ContributionEndpoint::Worktree {
            worktree_id: Some(worktree_id),
            id: None,
        }
    }

    #[tokio::test]
    async fn missing_initial_ids_emits_create_session_compat_counter() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_root = temp.path().join("workspace");
        init_git_repo_for_create_session_test(&workspace_root);
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
            metric.metric.labels.get("source").map(String::as_str) == Some("daemon")
                && metric.metric.labels.get("surface").map(String::as_str)
                    == Some("tasks.create_session")
                && metric.metric.labels.get("issue").map(String::as_str)
                    == Some("missing_initial_ids")
                && metric.metric.value >= 1.0
        }));
    }

    #[tokio::test]
    async fn created_sessions_record_agent_work_contributions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_root = temp.path().join("workspace");
        init_git_repo_for_create_session_test(&workspace_root);
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
        let handle = daemon.task_session_admission_handle_for_test();

        let parent_message_id = MessageId::new();
        let parent_turn_id = TurnId::new();
        let parent = handle
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
                    initial_prompt: Some("start parent".to_string()),
                    initial_message_id: Some(parent_message_id.0.to_string()),
                    initial_turn_id: Some(parent_turn_id.0.to_string()),
                    worktree_id: None,
                    execution_environment: None,
                    run_id_header: None,
                },
            )
            .await
            .expect("create parent session");
        let child_message_id = MessageId::new();
        let child_turn_id = TurnId::new();
        let child = handle
            .create_session_for_task(
                task.id,
                CreateTaskSessionInput {
                    id: None,
                    provider_id: "fake".to_string(),
                    model_id: "fake-model".to_string(),
                    reasoning_effort: None,
                    remember_model_preference: false,
                    parent_session_id: Some(parent.id.0.to_string()),
                    relationship: Some("subagent".to_string()),
                    initial_prompt: Some("start child".to_string()),
                    initial_message_id: Some(child_message_id.0.to_string()),
                    initial_turn_id: Some(child_turn_id.0.to_string()),
                    worktree_id: None,
                    execution_environment: None,
                    run_id_header: None,
                },
            )
            .await
            .expect("create child session");
        let store = daemon
            .store_for_workspace(workspace.id)
            .await
            .expect("workspace store");

        let contributions = store
            .list_workspace_contributions(workspace.id)
            .await
            .expect("list contributions");

        assert!(contributions.iter().any(|contribution| {
            contribution.subject == task_endpoint(task.id)
                && contribution.target == session_endpoint(parent.id)
                && contribution.source == RecordSource::Session
                && contribution.origin == RecordOrigin::System
        }));
        assert!(contributions.iter().any(|contribution| {
            contribution.subject == session_endpoint(parent.id)
                && contribution.target == worktree_endpoint(parent.worktree_id)
        }));
        assert!(contributions.iter().any(|contribution| {
            contribution.subject == task_endpoint(task.id)
                && contribution.target == session_endpoint(child.id)
        }));
        assert!(contributions.iter().any(|contribution| {
            contribution.subject == session_endpoint(child.id)
                && contribution.target == worktree_endpoint(child.worktree_id)
        }));
        assert!(contributions.iter().any(|contribution| {
            contribution.subject == session_endpoint(parent.id)
                && contribution.target == session_endpoint(child.id)
                && contribution
                    .metadata_json
                    .as_ref()
                    .and_then(|metadata| metadata.get("relationship"))
                    .and_then(serde_json::Value::as_str)
                    == Some("subagent")
        }));

        daemon.request_shutdown();
    }

    #[tokio::test]
    async fn requested_existing_sessions_backfill_agent_work_contribution_once() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_root = temp.path().join("workspace");
        init_git_repo_for_create_session_test(&workspace_root);
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
        let handle = daemon.task_session_admission_handle_for_test();
        let requested_session_id = SessionId::new();

        let create_input = |prompt: &str| CreateTaskSessionInput {
            id: Some(requested_session_id.0.to_string()),
            provider_id: "fake".to_string(),
            model_id: "fake-model".to_string(),
            reasoning_effort: None,
            remember_model_preference: false,
            parent_session_id: None,
            relationship: None,
            initial_prompt: Some(prompt.to_string()),
            initial_message_id: Some(MessageId::new().0.to_string()),
            initial_turn_id: Some(TurnId::new().0.to_string()),
            worktree_id: None,
            execution_environment: None,
            run_id_header: None,
        };

        let created = handle
            .create_session_for_task(task.id, create_input("start requested session"))
            .await
            .expect("create requested session");
        let existing = handle
            .create_session_for_task(task.id, create_input("retry requested session"))
            .await
            .expect("return existing requested session");

        assert_eq!(created.id, requested_session_id);
        assert_eq!(existing.id, requested_session_id);

        let store = daemon
            .store_for_workspace(workspace.id)
            .await
            .expect("workspace store");
        let contributions = store
            .list_workspace_contributions(workspace.id)
            .await
            .expect("list contributions");
        let task_session_links = contributions
            .iter()
            .filter(|contribution| {
                contribution.subject == task_endpoint(task.id)
                    && contribution.target == session_endpoint(requested_session_id)
            })
            .count();
        let session_worktree_links = contributions
            .iter()
            .filter(|contribution| {
                contribution.subject == session_endpoint(requested_session_id)
                    && contribution.target == worktree_endpoint(created.worktree_id)
            })
            .count();

        assert_eq!(task_session_links, 1);
        assert_eq!(session_worktree_links, 1);

        daemon.request_shutdown();
    }

    #[tokio::test]
    async fn missing_parent_session_still_records_direct_agent_work_links() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_root = temp.path().join("workspace");
        init_git_repo_for_create_session_test(&workspace_root);
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
        let missing_parent_id = SessionId::new();
        let child = daemon
            .task_session_admission_handle_for_test()
            .create_session_for_task(
                task.id,
                CreateTaskSessionInput {
                    id: None,
                    provider_id: "fake".to_string(),
                    model_id: "fake-model".to_string(),
                    reasoning_effort: None,
                    remember_model_preference: false,
                    parent_session_id: Some(missing_parent_id.0.to_string()),
                    relationship: Some("subagent".to_string()),
                    initial_prompt: Some("start orphan child".to_string()),
                    initial_message_id: Some(MessageId::new().0.to_string()),
                    initial_turn_id: Some(TurnId::new().0.to_string()),
                    worktree_id: None,
                    execution_environment: None,
                    run_id_header: None,
                },
            )
            .await
            .expect("create child session with unresolved parent");

        let store = daemon
            .store_for_workspace(workspace.id)
            .await
            .expect("workspace store");
        let contributions = store
            .list_workspace_contributions(workspace.id)
            .await
            .expect("list contributions");
        let child_endpoint = session_endpoint(child.id);
        let worktree_endpoint = worktree_endpoint(child.worktree_id);
        assert!(contributions.iter().any(|contribution| {
            contribution.subject == task_endpoint(task.id) && contribution.target == child_endpoint
        }));
        assert!(contributions.iter().any(|contribution| {
            contribution.subject == child_endpoint && contribution.target == worktree_endpoint
        }));
        assert!(contributions.iter().all(|contribution| {
            contribution.subject != session_endpoint(missing_parent_id)
                && contribution.target != session_endpoint(missing_parent_id)
        }));

        daemon.request_shutdown();
    }
}
