use ctx_route_contracts::terminals::{
    CreateTerminalRouteRequest, CreateTerminalRouteSpec, DeleteTerminalRouteParams,
    ListWorkspaceTerminalsRouteParams, MintTerminalStreamTokenRouteParams, TerminalRouteError,
    TerminalSessionRouteResponse, TerminalStreamConnectRouteResponse, TerminalStreamRouteParams,
};
use ctx_transport_runtime::terminal_launch::{TerminalLaunchError, TerminalLaunchErrorKind};
use ctx_transport_runtime::terminals::TerminalStreamAccessError;

use crate::daemon::TerminalRouteHandle;

use super::{launch::CreateTerminalLaunchRequest, TerminalStreamRouteAdmission};

fn create_terminal_launch_request(spec: CreateTerminalRouteSpec) -> CreateTerminalLaunchRequest {
    CreateTerminalLaunchRequest {
        workspace_id: spec.workspace_id,
        task_id: spec.task_id,
        session_id: spec.session_id,
        worktree_id: spec.worktree_id,
        cwd: spec.cwd,
        shell: spec.shell,
    }
}

fn terminal_launch_route_error(error: TerminalLaunchError) -> TerminalRouteError {
    match error.kind() {
        TerminalLaunchErrorKind::BadRequest => TerminalRouteError::bad_request(error.message()),
        TerminalLaunchErrorKind::NotFound => TerminalRouteError::not_found(error.message()),
        TerminalLaunchErrorKind::Internal => TerminalRouteError::internal(error.message()),
    }
}

impl TerminalRouteHandle {
    pub async fn list_workspace_terminal_responses_for_route(
        &self,
        params: ListWorkspaceTerminalsRouteParams,
    ) -> Result<Vec<TerminalSessionRouteResponse>, TerminalRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        let sessions = super::list_workspace_terminals(self.terminals(), workspace_id).await;
        Ok(sessions.into_iter().map(Into::into).collect())
    }

    pub async fn create_workspace_terminal_for_route(
        &self,
        raw_workspace_id: &str,
        req: CreateTerminalRouteRequest,
    ) -> Result<TerminalSessionRouteResponse, TerminalRouteError> {
        let launch_req = create_terminal_launch_request(req.parse(raw_workspace_id)?);
        self.create_terminal(launch_req)
            .await
            .map(Into::into)
            .map_err(terminal_launch_route_error)
    }

    pub async fn delete_terminal_for_route(
        &self,
        params: DeleteTerminalRouteParams,
    ) -> Result<(), TerminalRouteError> {
        let terminal_id = params.parse_terminal_id()?;
        if super::delete_terminal(self.terminals(), terminal_id).await {
            return Ok(());
        }
        Err(TerminalRouteError::not_found("terminal not found"))
    }

    pub async fn mint_terminal_stream_token_for_route(
        &self,
        params: MintTerminalStreamTokenRouteParams,
    ) -> Result<TerminalStreamConnectRouteResponse, TerminalRouteError> {
        let terminal_id = params.parse_terminal_id()?;
        let token = super::mint_terminal_stream_token(self.terminals(), terminal_id)
            .await
            .ok_or_else(|| TerminalRouteError::not_found("terminal not found"))?;
        Ok(TerminalStreamConnectRouteResponse {
            stream_path: token.stream_path,
            expires_at: token.expires_at,
        })
    }

    pub async fn admit_terminal_stream_for_route(
        &self,
        params: TerminalStreamRouteParams,
    ) -> Result<TerminalStreamRouteAdmission, TerminalRouteError> {
        let terminal_id = params.parse_terminal_id()?;
        let tail_bytes = params.tail_bytes();
        let token = params
            .token()
            .ok_or_else(terminal_stream_missing_token_route_error)?;
        let session = self
            .terminals()
            .require_stream_access(terminal_id, token)
            .await
            .map_err(terminal_stream_access_route_error)?;

        Ok(TerminalStreamRouteAdmission {
            session,
            tail_bytes,
        })
    }
}

fn terminal_stream_missing_token_route_error() -> TerminalRouteError {
    TerminalRouteError::unauthorized("terminal stream token required")
}

fn terminal_stream_access_route_error(error: TerminalStreamAccessError) -> TerminalRouteError {
    match error {
        TerminalStreamAccessError::Unauthorized => {
            TerminalRouteError::unauthorized("terminal stream token required")
        }
        TerminalStreamAccessError::NotFound => TerminalRouteError::not_found("terminal not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use ctx_core::ids::{TerminalId, WorkspaceId};
    use ctx_core::models::TerminalStatus;
    use ctx_route_contracts::terminals::TerminalRouteErrorKind;
    use ctx_transport_runtime::terminals::{TerminalCreateRequest, TerminalManager};

    fn route_handle_with_create_effect(
        terminals: Arc<TerminalManager>,
        create_terminal: impl Fn(
                CreateTerminalLaunchRequest,
            ) -> crate::daemon::launch_route_handles::CreateTerminalFuture
            + Send
            + Sync
            + 'static,
    ) -> TerminalRouteHandle {
        TerminalRouteHandle::new_for_test(terminals, Arc::new(create_terminal))
    }

    fn route_handle(terminals: Arc<TerminalManager>) -> TerminalRouteHandle {
        route_handle_with_create_effect(terminals, |_req| {
            Box::pin(async {
                Err(TerminalLaunchError::internal(
                    "test route handle should not launch terminals",
                ))
            }) as crate::daemon::launch_route_handles::CreateTerminalFuture
        })
    }

    #[test]
    fn terminal_stream_access_errors_map_to_route_errors() {
        let route_error = terminal_stream_missing_token_route_error();
        assert_eq!(route_error.kind(), TerminalRouteErrorKind::Unauthorized);
        assert_eq!(route_error.message(), "terminal stream token required");

        let route_error =
            terminal_stream_access_route_error(TerminalStreamAccessError::Unauthorized);
        assert_eq!(route_error.kind(), TerminalRouteErrorKind::Unauthorized);
        assert_eq!(route_error.message(), "terminal stream token required");

        let route_error = terminal_stream_access_route_error(TerminalStreamAccessError::NotFound);
        assert_eq!(route_error.kind(), TerminalRouteErrorKind::NotFound);
        assert_eq!(route_error.message(), "terminal not found");
    }

    #[tokio::test]
    async fn terminal_stream_route_checks_missing_token_before_terminal_lookup() {
        let handle = route_handle(Arc::new(TerminalManager::default()));
        let missing_terminal_id = TerminalId::new();

        let result = handle
            .admit_terminal_stream_for_route(TerminalStreamRouteParams::new(
                missing_terminal_id.0.to_string(),
                None,
                None,
            ))
            .await;
        let error = match result {
            Ok(_) => panic!("missing token should reject before terminal lookup"),
            Err(error) => error,
        };

        assert_eq!(error.kind(), TerminalRouteErrorKind::Unauthorized);
        assert_eq!(error.message(), "terminal stream token required");
    }

    #[tokio::test]
    async fn create_terminal_rejects_invalid_workspace_before_launch_effect() {
        let launch_calls = Arc::new(AtomicUsize::new(0));
        let handle = route_handle_with_create_effect(Arc::new(TerminalManager::default()), {
            let launch_calls = Arc::clone(&launch_calls);
            move |_req| {
                launch_calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async {
                    Err(TerminalLaunchError::internal(
                        "invalid route params should reject before launch",
                    ))
                }) as crate::daemon::launch_route_handles::CreateTerminalFuture
            }
        });

        let result = handle
            .create_workspace_terminal_for_route(
                "not-a-workspace-id",
                CreateTerminalRouteRequest {
                    task_id: None,
                    session_id: None,
                    worktree_id: None,
                    cwd: None,
                    shell: None,
                },
            )
            .await;
        let error = match result {
            Ok(_) => panic!("invalid workspace id should reject"),
            Err(error) => error,
        };

        assert_eq!(error.kind(), TerminalRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "invalid workspace id");
        assert_eq!(launch_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn create_terminal_launch_errors_map_to_route_errors() {
        let handle =
            route_handle_with_create_effect(Arc::new(TerminalManager::default()), |_req| {
                Box::pin(async { Err(TerminalLaunchError::not_found("workspace missing")) })
                    as crate::daemon::launch_route_handles::CreateTerminalFuture
            });

        let result = handle
            .create_workspace_terminal_for_route(
                &WorkspaceId::new().0.to_string(),
                CreateTerminalRouteRequest {
                    task_id: None,
                    session_id: None,
                    worktree_id: None,
                    cwd: None,
                    shell: None,
                },
            )
            .await;
        let error = match result {
            Ok(_) => panic!("launch error should map to route error"),
            Err(error) => error,
        };

        assert_eq!(error.kind(), TerminalRouteErrorKind::NotFound);
        assert_eq!(error.message(), "workspace missing");
    }

    #[tokio::test]
    async fn delete_terminal_removes_kills_and_marks_session_exited() {
        let temp = tempfile::tempdir().expect("tempdir");
        let terminals = Arc::new(TerminalManager::default());
        let handle = route_handle(Arc::clone(&terminals));
        let session = terminals
            .create(TerminalCreateRequest {
                workspace_id: WorkspaceId::new(),
                task_id: None,
                session_id: None,
                worktree_id: None,
                cwd: temp.path().to_path_buf(),
                shell: "/bin/sh".to_string(),
                cols: None,
                rows: None,
                env: HashMap::new(),
                native_container: None,
                shared_vm_container: None,
            })
            .await
            .expect("terminal session");
        let terminal_id = session.snapshot().id;

        handle
            .delete_terminal_for_route(DeleteTerminalRouteParams::new(terminal_id.0.to_string()))
            .await
            .expect("delete terminal");

        assert!(terminals.get(terminal_id).await.is_none());
        assert!(matches!(session.snapshot().status, TerminalStatus::Exited));
    }
}
