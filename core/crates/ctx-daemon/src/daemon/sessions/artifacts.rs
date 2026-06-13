use ctx_core::models::Artifact;
use ctx_observability::logs;
use ctx_route_contracts::sessions::{parse_session_route_id, SessionRouteParams};
use ctx_session_artifacts::route_contract::{
    SessionArtifactDownloadRouteParams, SessionArtifactInput, SessionArtifactRouteError,
    SessionArtifactsRouteResponse, SetSessionArtifactsRouteRequest,
};

use crate::daemon::route_files::{open_canonical_route_file, RouteFileDownloadError};
use crate::daemon::SessionArtifactsHandle;
use crate::daemon::{ScopedMcpSessionAccessError, SessionStoreAccessError};
use ctx_core::ids::{ArtifactId, SessionId};

#[derive(Debug)]
pub struct SessionArtifactDownload {
    pub file: tokio::fs::File,
    pub size: u64,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub mime_type: String,
    pub name: Option<String>,
}

impl SessionArtifactsHandle {
    pub async fn list_session_artifacts_with_missing_for_route_params(
        &self,
        params: SessionRouteParams,
    ) -> Result<SessionArtifactsRouteResponse, SessionArtifactRouteError> {
        let session_id = parse_session_artifact_route_session_id(params.session_id())?;
        self.list_session_artifacts_with_missing_for_route(session_id)
            .await
            .map(Into::into)
    }

    pub async fn list_session_artifacts_with_missing_for_route(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<Artifact>, SessionArtifactRouteError> {
        let store = self
            .existing_session_store(session_id)
            .await
            .map_err(session_artifact_store_error)?;
        let session = store
            .get_session(session_id)
            .await
            .map_err(session_artifact_internal_error)?
            .ok_or(SessionArtifactRouteError::NotFound)?;
        let session_spool_dir = self.session_tool_output_spool_dir(session.id);
        ctx_session_artifacts::list_session_artifacts_with_missing(
            &store,
            &session,
            &session_spool_dir,
        )
        .await
        .map_err(session_artifact_service_error)
    }

    pub async fn set_session_artifacts_for_route_params(
        &self,
        params: SessionRouteParams,
        mcp_auth: Option<ctx_mcp_auth::McpAuthContext>,
        request: SetSessionArtifactsRouteRequest,
    ) -> Result<SessionArtifactsRouteResponse, SessionArtifactRouteError> {
        let session_id = parse_session_artifact_route_session_id(params.session_id())?;
        if let Some(mcp_auth) = mcp_auth {
            self.require_scoped_mcp_session_context(mcp_auth, session_id)
                .await
                .map_err(scoped_mcp_session_artifact_route_error)?;
        }
        self.set_session_artifacts_for_route(session_id, request.into_artifacts())
            .await
            .map(Into::into)
    }

    pub async fn set_session_artifacts_for_route(
        &self,
        session_id: SessionId,
        inputs: Vec<SessionArtifactInput>,
    ) -> Result<Vec<Artifact>, SessionArtifactRouteError> {
        let store = self
            .existing_session_store_for_write(session_id)
            .await
            .map_err(session_artifact_store_error)?;
        let session = store
            .get_session(session_id)
            .await
            .map_err(session_artifact_internal_error)?
            .ok_or(SessionArtifactRouteError::NotFound)?;
        let service_inputs: Vec<ctx_session_artifacts::SessionArtifactInput> =
            inputs.into_iter().map(Into::into).collect();
        let session_spool_dir = self.session_tool_output_spool_dir(session.id);
        let artifacts = ctx_session_artifacts::build_session_artifacts(
            &store,
            &session,
            &session_spool_dir,
            service_inputs,
        )
        .await
        .map_err(session_artifact_service_error)?;
        self.replace_session_artifacts_and_publish(&session, &artifacts)
            .await
            .map_err(session_artifact_internal_error)?;
        Ok(artifacts)
    }

    pub async fn open_session_artifact_for_route_params(
        &self,
        params: SessionArtifactDownloadRouteParams,
    ) -> Result<SessionArtifactDownload, SessionArtifactRouteError> {
        let session_id = parse_session_artifact_route_session_id(params.session_id())?;
        let artifact_id = parse_session_artifact_route_artifact_id(params.artifact_id())?;
        self.open_session_artifact_for_route(session_id, artifact_id)
            .await
    }

    pub async fn open_session_artifact_for_route(
        &self,
        session_id: SessionId,
        artifact_id: ArtifactId,
    ) -> Result<SessionArtifactDownload, SessionArtifactRouteError> {
        let store = self
            .existing_session_store(session_id)
            .await
            .map_err(session_artifact_store_error)?;
        let session = store
            .get_session(session_id)
            .await
            .map_err(session_artifact_internal_error)?
            .ok_or(SessionArtifactRouteError::NotFound)?;
        let session_spool_dir = self.session_tool_output_spool_dir(session.id);
        let download = ctx_session_artifacts::resolve_session_artifact_download(
            &store,
            &session,
            &session_spool_dir,
            artifact_id,
        )
        .await
        .map_err(session_artifact_service_error)?;
        let file = open_canonical_route_file(&download.canonical_path)
            .await
            .map_err(session_artifact_file_error)?;
        let meta = file
            .metadata()
            .await
            .map_err(|_| SessionArtifactRouteError::NotFound)?;
        if !meta.is_file() {
            return Err(SessionArtifactRouteError::NotFound);
        }
        let size = meta.len();
        let modified = meta.modified().ok();
        let etag = modified.and_then(|modified| {
            ctx_session_artifacts::build_session_artifact_etag(size, modified)
        });
        let last_modified =
            modified.map(ctx_session_artifacts::build_session_artifact_last_modified);
        Ok(SessionArtifactDownload {
            file,
            size,
            etag,
            last_modified,
            mime_type: download.mime_type,
            name: download.name,
        })
    }
}

fn parse_session_artifact_route_session_id(
    value: &str,
) -> Result<SessionId, SessionArtifactRouteError> {
    parse_session_route_id(value)
        .map_err(|_| SessionArtifactRouteError::BadRequest("invalid session id".to_string()))
}

fn parse_session_artifact_route_artifact_id(
    value: &str,
) -> Result<ArtifactId, SessionArtifactRouteError> {
    uuid::Uuid::parse_str(value)
        .map(ArtifactId)
        .map_err(|_| SessionArtifactRouteError::BadRequest("invalid artifact id".to_string()))
}

fn scoped_mcp_session_artifact_route_error(
    error: ScopedMcpSessionAccessError,
) -> SessionArtifactRouteError {
    match error {
        ScopedMcpSessionAccessError::Unauthorized(message) => {
            SessionArtifactRouteError::Unauthorized(message.to_string())
        }
        ScopedMcpSessionAccessError::SessionNotFound => SessionArtifactRouteError::NotFound,
        ScopedMcpSessionAccessError::StoreUnavailable(error) => {
            session_artifact_internal_error(error)
        }
    }
}

fn session_artifact_store_error(error: SessionStoreAccessError) -> SessionArtifactRouteError {
    match error {
        SessionStoreAccessError::NotFound => SessionArtifactRouteError::NotFound,
        SessionStoreAccessError::LookupUnavailable(error) => session_artifact_internal_error(error),
        SessionStoreAccessError::StoreUnavailable => {
            SessionArtifactRouteError::Internal("workspace store unavailable".to_string())
        }
    }
}

fn session_artifact_service_error(
    error: ctx_session_artifacts::SessionArtifactError,
) -> SessionArtifactRouteError {
    match error {
        ctx_session_artifacts::SessionArtifactError::NotFound => {
            SessionArtifactRouteError::NotFound
        }
        ctx_session_artifacts::SessionArtifactError::BadRequest(message) => {
            SessionArtifactRouteError::BadRequest(logs::redact_sensitive(&message))
        }
        ctx_session_artifacts::SessionArtifactError::Internal(message) => {
            SessionArtifactRouteError::Internal(logs::redact_sensitive(&message))
        }
    }
}

fn session_artifact_internal_error(error: impl std::fmt::Display) -> SessionArtifactRouteError {
    SessionArtifactRouteError::Internal(logs::redact_sensitive(&error.to_string()))
}

fn session_artifact_file_error(error: RouteFileDownloadError) -> SessionArtifactRouteError {
    match error {
        RouteFileDownloadError::NotFound => SessionArtifactRouteError::NotFound,
        RouteFileDownloadError::Internal => {
            SessionArtifactRouteError::Internal("failed to open session artifact".to_string())
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_support::{TaskLifecycleSessionSeed, TaskLifecycleWorktreeSeed, TestDaemon};
    use ctx_core::ids::WorktreeId;
    use ctx_core::models::{ExecutionEnvironment, Session, SessionEventType, VcsKind};
    use ctx_mcp_auth::{McpAuthCapabilities, McpAuthContext};

    struct ArtifactFixture {
        _temp: tempfile::TempDir,
        daemon: TestDaemon,
        session: Session,
        artifact_path: std::path::PathBuf,
    }

    async fn seeded_artifact_fixture() -> ArtifactFixture {
        let temp = tempfile::tempdir().expect("tempdir");
        let daemon = TestDaemon::new_for_test(
            temp.path().join("data"),
            "http://127.0.0.1:4310".to_string(),
        )
        .await
        .expect("daemon");
        let workspace_root = temp.path().join("repo");
        let worktree_root = temp.path().join("worktree");
        std::fs::create_dir_all(&workspace_root).expect("workspace root");
        std::fs::create_dir_all(&worktree_root).expect("worktree root");
        let workspace = daemon
            .seed_task_lifecycle_workspace_for_test("ws", &workspace_root, VcsKind::Git)
            .await
            .expect("workspace");
        let task = daemon
            .seed_task_lifecycle_task_for_test(workspace.id, "task")
            .await
            .expect("task");
        let worktree_id = WorktreeId::new();
        let worktree = daemon
            .seed_task_lifecycle_worktree_for_test(TaskLifecycleWorktreeSeed {
                workspace_id: workspace.id,
                owner_task_id: task.id,
                worktree_id,
                root_path: worktree_root.clone(),
                base_commit: "base".to_string(),
                git_branch: "task-branch".to_string(),
                make_primary: true,
            })
            .await
            .expect("worktree");
        let session = daemon
            .seed_task_lifecycle_session_for_test(TaskLifecycleSessionSeed {
                task_id: task.id,
                workspace_id: workspace.id,
                worktree_id: worktree.id,
                execution_environment: ExecutionEnvironment::Host,
                title: "session".to_string(),
                parent_session_id: None,
                role: None,
            })
            .await
            .expect("session");
        let artifact_path = worktree_root.join("artifact.txt");
        std::fs::write(&artifact_path, b"artifact").expect("artifact file");
        ArtifactFixture {
            _temp: temp,
            daemon,
            session,
            artifact_path,
        }
    }

    fn scoped_context_for(session: &Session) -> McpAuthContext {
        McpAuthContext {
            session_id: session.id,
            workspace_id: session.workspace_id,
            worktree_id: session.worktree_id,
            capabilities: McpAuthCapabilities::provider_session(),
        }
    }

    fn set_request(path: &std::path::Path) -> SetSessionArtifactsRouteRequest {
        SetSessionArtifactsRouteRequest::new(vec![SessionArtifactInput::new(
            path.to_string_lossy().to_string(),
            None,
            None,
        )])
    }

    #[tokio::test]
    async fn open_canonical_route_file_rejects_symlink_swap() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_path = outside.path().join("outside.txt");
        std::fs::write(&outside_path, b"outside\n").unwrap();

        let artifact_path = root.path().join("artifact.txt");
        std::fs::write(&artifact_path, b"inside\n").unwrap();
        let canonical = tokio::fs::canonicalize(&artifact_path).await.unwrap();

        let renamed = root.path().join("artifact.saved");
        std::fs::rename(&artifact_path, &renamed).unwrap();
        std::os::unix::fs::symlink(&outside_path, &artifact_path).unwrap();

        let err = open_canonical_route_file(&canonical).await.unwrap_err();
        assert_eq!(err, RouteFileDownloadError::NotFound);
    }

    #[test]
    fn route_params_reject_invalid_session_and_artifact_ids() {
        let error = parse_session_artifact_route_session_id("not-a-session").unwrap_err();
        assert_eq!(
            error,
            SessionArtifactRouteError::BadRequest("invalid session id".to_string())
        );

        let error = parse_session_artifact_route_artifact_id("not-an-artifact").unwrap_err();
        assert_eq!(
            error,
            SessionArtifactRouteError::BadRequest("invalid artifact id".to_string())
        );
    }

    #[test]
    fn scoped_mcp_artifact_route_errors_preserve_public_mapping() {
        let unauthorized = scoped_mcp_session_artifact_route_error(
            ScopedMcpSessionAccessError::Unauthorized("scoped message"),
        );
        assert_eq!(
            unauthorized,
            SessionArtifactRouteError::Unauthorized("scoped message".to_string())
        );

        let missing =
            scoped_mcp_session_artifact_route_error(ScopedMcpSessionAccessError::SessionNotFound);
        assert_eq!(missing, SessionArtifactRouteError::NotFound);
    }

    #[tokio::test]
    async fn set_session_artifacts_persists_state_and_artifacts_set_event() {
        let fixture = seeded_artifact_fixture().await;
        let handle = fixture.daemon.session_artifacts_handle_for_test();

        let artifacts = handle
            .set_session_artifacts_for_route_params(
                SessionRouteParams::new(fixture.session.id.0.to_string()),
                None,
                set_request(&fixture.artifact_path),
            )
            .await
            .expect("set artifacts")
            .into_artifacts();

        assert_eq!(artifacts.len(), 1);
        let store = handle
            .existing_session_store(fixture.session.id)
            .await
            .expect("session store");
        let state = store
            .get_session_state(fixture.session.id)
            .await
            .expect("session state");
        assert_eq!(state.artifacts.len(), 1);
        let events = store
            .list_session_events(fixture.session.id)
            .await
            .expect("session events");
        assert!(
            events
                .iter()
                .any(|event| matches!(event.event_type, SessionEventType::ArtifactsSet)),
            "expected artifacts_set event, got {events:?}"
        );
    }

    #[tokio::test]
    async fn artifact_routes_hide_archived_subagents() {
        let fixture = seeded_artifact_fixture().await;
        let child = fixture
            .daemon
            .seed_task_lifecycle_session_for_test(TaskLifecycleSessionSeed {
                task_id: fixture.session.task_id,
                workspace_id: fixture.session.workspace_id,
                worktree_id: fixture.session.worktree_id,
                execution_environment: fixture.session.execution_environment,
                title: "subagent".to_string(),
                parent_session_id: Some(fixture.session.id),
                role: Some("sub_agent".to_string()),
            })
            .await
            .expect("child session");
        assert!(fixture
            .daemon
            .archive_task_lifecycle_subagent_session_for_test(
                fixture.session.workspace_id,
                fixture.session.id,
                child.id,
            )
            .await
            .expect("archive child"));

        let error = fixture
            .daemon
            .session_artifacts_handle_for_test()
            .list_session_artifacts_with_missing_for_route_params(SessionRouteParams::new(
                child.id.0.to_string(),
            ))
            .await
            .expect_err("archived subagent should be hidden");

        assert_eq!(error, SessionArtifactRouteError::NotFound);
    }

    #[tokio::test]
    async fn artifact_routes_hide_deleting_workspace() {
        let fixture = seeded_artifact_fixture().await;
        fixture
            .daemon
            .cache_rehydration_begin_workspace_delete_for_test(fixture.session.workspace_id)
            .await;

        let error = fixture
            .daemon
            .session_artifacts_handle_for_test()
            .list_session_artifacts_with_missing_for_route_params(SessionRouteParams::new(
                fixture.session.id.0.to_string(),
            ))
            .await
            .expect_err("deleting workspace should hide session");

        fixture
            .daemon
            .cache_rehydration_finish_workspace_delete_for_test(fixture.session.workspace_id)
            .await;
        assert_eq!(error, SessionArtifactRouteError::NotFound);
    }

    #[tokio::test]
    async fn artifact_routes_report_permanent_store_open_failure_as_internal() {
        let fixture = seeded_artifact_fixture().await;
        fixture
            .daemon
            .cache_rehydration_make_workspace_store_unopenable_for_test(
                fixture.session.workspace_id,
            )
            .await
            .expect("make store unavailable");

        let error = fixture
            .daemon
            .session_artifacts_handle_for_test()
            .set_session_artifacts_for_route_params(
                SessionRouteParams::new(fixture.session.id.0.to_string()),
                None,
                set_request(&fixture.artifact_path),
            )
            .await
            .expect_err("permanent store-open failure should be internal");

        assert!(
            matches!(error, SessionArtifactRouteError::Internal(_)),
            "unexpected error: {error:?}"
        );
    }

    #[tokio::test]
    async fn scoped_mcp_artifact_validation_checks_loaded_session_scope() {
        let fixture = seeded_artifact_fixture().await;
        let mut context = scoped_context_for(&fixture.session);
        context.workspace_id = ctx_core::ids::WorkspaceId::new();

        let error = fixture
            .daemon
            .session_artifacts_handle_for_test()
            .set_session_artifacts_for_route_params(
                SessionRouteParams::new(fixture.session.id.0.to_string()),
                Some(context),
                set_request(&fixture.artifact_path),
            )
            .await
            .expect_err("loaded scope mismatch should be unauthorized");

        assert_eq!(
            error,
            SessionArtifactRouteError::Unauthorized(
                "scoped ctx-mcp token does not match the loaded session scope".to_string()
            )
        );
    }
}
