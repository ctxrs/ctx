use ctx_transport_runtime::web_sessions::{
    WebSessionActionError, WebSessionInfo, WebSessionRunResponse,
};

use crate::daemon::WebSessionRouteHandle;

use super::{WebSessionLaunchError, WebSessionLaunchErrorKind, WebSessionLaunchRequest};

use ctx_route_contracts::web_sessions::{
    WebSessionActionRouteRequest, WebSessionCreateRouteRequest, WebSessionCreateRouteSpec,
    WebSessionListRouteQuery, WebSessionRouteError,
};

fn web_session_launch_request(spec: WebSessionCreateRouteSpec) -> WebSessionLaunchRequest {
    WebSessionLaunchRequest {
        session_id: spec.session_id,
        worktree_id: spec.worktree_id,
        url: spec.url,
        viewport: spec.viewport,
        fps: spec.fps,
    }
}

fn web_session_launch_route_error(error: WebSessionLaunchError) -> WebSessionRouteError {
    match error.kind() {
        WebSessionLaunchErrorKind::BadRequest => WebSessionRouteError::bad_request(error.message()),
        WebSessionLaunchErrorKind::Forbidden => WebSessionRouteError::forbidden(error.message()),
        WebSessionLaunchErrorKind::Internal => WebSessionRouteError::internal(error.message()),
    }
}

fn web_session_action_route_error(error: WebSessionActionError) -> WebSessionRouteError {
    match error {
        WebSessionActionError::NotFound => WebSessionRouteError::not_found("web session not found"),
        WebSessionActionError::Internal => {
            WebSessionRouteError::internal("web session action failed")
        }
    }
}

impl WebSessionRouteHandle {
    pub async fn create_web_session_for_route(
        &self,
        request: WebSessionCreateRouteRequest,
    ) -> Result<WebSessionInfo, WebSessionRouteError> {
        let request = web_session_launch_request(request.validate()?);
        self.create_web_session(request)
            .await
            .map_err(web_session_launch_route_error)
    }

    pub async fn list_web_sessions_for_route(
        &self,
        query: WebSessionListRouteQuery,
    ) -> Result<Vec<WebSessionInfo>, WebSessionRouteError> {
        let session_id = query.validated_session_id()?;
        let mut sessions = self.web_sessions().list().await;
        if let Some(session_id) = session_id {
            sessions.retain(|session| session.session_id.as_deref() == Some(session_id));
        }
        Ok(sessions)
    }

    pub async fn get_web_session_for_route(
        &self,
        id: &str,
    ) -> Result<WebSessionInfo, WebSessionRouteError> {
        self.web_sessions()
            .get_info(id)
            .await
            .ok_or_else(|| WebSessionRouteError::not_found("web session not found"))
    }

    pub async fn run_web_session_for_route(
        &self,
        id: &str,
        request: WebSessionActionRouteRequest,
    ) -> Result<WebSessionRunResponse, WebSessionRouteError> {
        self.web_sessions()
            .run_action(id, request.into_run_request())
            .await
            .map_err(web_session_action_route_error)
    }

    pub async fn eval_web_session_for_route(
        &self,
        id: &str,
        request: WebSessionActionRouteRequest,
    ) -> Result<WebSessionRunResponse, WebSessionRouteError> {
        self.web_sessions()
            .eval_action(id, request.into_run_request())
            .await
            .map_err(web_session_action_route_error)
    }

    pub async fn close_web_session_for_route(&self, id: &str) -> Result<(), WebSessionRouteError> {
        self.web_sessions()
            .close_action(id)
            .await
            .map_err(web_session_action_route_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use chrono::Utc;
    use ctx_route_contracts::web_sessions::WebSessionRouteErrorKind;
    use ctx_transport_runtime::web_sessions::{
        WebSessionAccessError, WebSessionManager, WebSessionStatus, WebSessionViewport,
    };

    fn route_handle_with_create_effect(
        web_sessions: Arc<WebSessionManager>,
        create_web_session: impl Fn(
                WebSessionLaunchRequest,
            ) -> crate::daemon::launch_route_handles::CreateWebSessionFuture
            + Send
            + Sync
            + 'static,
    ) -> WebSessionRouteHandle {
        WebSessionRouteHandle::new_for_test(web_sessions, Arc::new(create_web_session))
    }

    fn route_handle(web_sessions: Arc<WebSessionManager>) -> WebSessionRouteHandle {
        route_handle_with_create_effect(web_sessions, |_req| {
            Box::pin(async {
                Err(WebSessionLaunchError::internal(
                    "test route handle should not launch web sessions",
                ))
            }) as crate::daemon::launch_route_handles::CreateWebSessionFuture
        })
    }

    fn fake_web_session_info(id: &str) -> WebSessionInfo {
        let now = Utc::now();
        WebSessionInfo {
            id: id.to_string(),
            kind: "web".to_string(),
            session_id: None,
            worktree_id: None,
            status: WebSessionStatus::Running,
            created_at: now,
            updated_at: now,
            last_activity: now,
            url: "https://example.test".to_string(),
            viewport: WebSessionViewport {
                width: 1280,
                height: 720,
            },
            fps: 30,
            viewers: 0,
            stream_path: format!("/sessions/web/{id}/view"),
            stream_url: None,
        }
    }

    #[test]
    fn action_errors_map_to_route_status_classes() {
        let not_found = web_session_action_route_error(WebSessionActionError::NotFound);
        assert_eq!(not_found.kind(), WebSessionRouteErrorKind::NotFound);

        let internal = web_session_action_route_error(WebSessionActionError::Internal);
        assert_eq!(internal.kind(), WebSessionRouteErrorKind::Internal);
    }

    #[tokio::test]
    async fn create_web_session_rejects_invalid_request_before_launch_effect() {
        let launch_calls = Arc::new(AtomicUsize::new(0));
        let handle = route_handle_with_create_effect(Arc::new(WebSessionManager::new()), {
            let launch_calls = Arc::clone(&launch_calls);
            move |_req| {
                launch_calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async {
                    Err(WebSessionLaunchError::internal(
                        "invalid request should reject before launch",
                    ))
                }) as crate::daemon::launch_route_handles::CreateWebSessionFuture
            }
        });

        let result = handle
            .create_web_session_for_route(WebSessionCreateRouteRequest {
                session_id: Some("not-a-uuid".to_string()),
                worktree_id: None,
                url: "https://example.test".to_string(),
                viewport: None,
                fps: None,
            })
            .await;
        let error = match result {
            Ok(_) => panic!("invalid route request should reject"),
            Err(error) => error,
        };

        assert_eq!(error.kind(), WebSessionRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "invalid session id");
        assert_eq!(launch_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn create_web_session_launch_errors_map_to_route_errors() {
        let handle = route_handle_with_create_effect(Arc::new(WebSessionManager::new()), |_req| {
            Box::pin(async { Err(WebSessionLaunchError::forbidden("sandbox-only session")) })
                as crate::daemon::launch_route_handles::CreateWebSessionFuture
        });

        let result = handle
            .create_web_session_for_route(WebSessionCreateRouteRequest {
                session_id: None,
                worktree_id: None,
                url: "https://example.test".to_string(),
                viewport: None,
                fps: None,
            })
            .await;
        let error = match result {
            Ok(_) => panic!("launch error should map to route error"),
            Err(error) => error,
        };

        assert_eq!(error.kind(), WebSessionRouteErrorKind::Forbidden);
        assert_eq!(error.message(), "sandbox-only session");
    }

    #[tokio::test]
    async fn create_web_session_uses_injected_launch_effect() {
        let handle = route_handle_with_create_effect(Arc::new(WebSessionManager::new()), |_req| {
            Box::pin(async { Ok(fake_web_session_info("web-session-1")) })
                as crate::daemon::launch_route_handles::CreateWebSessionFuture
        });

        let created = handle
            .create_web_session_for_route(WebSessionCreateRouteRequest {
                session_id: None,
                worktree_id: None,
                url: "https://example.test".to_string(),
                viewport: None,
                fps: None,
            })
            .await
            .expect("web session creation");

        assert_eq!(created.id, "web-session-1");
    }

    #[tokio::test]
    async fn web_session_list_rejects_invalid_session_filter() {
        let handle = route_handle(Arc::new(WebSessionManager::new()));

        let result = handle
            .list_web_sessions_for_route(WebSessionListRouteQuery {
                session_id: Some("not-a-uuid".to_string()),
            })
            .await;
        let error = match result {
            Ok(_) => panic!("invalid list filter should reject"),
            Err(error) => error,
        };

        assert_eq!(error.kind(), WebSessionRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "invalid session id");
    }

    #[tokio::test]
    async fn web_session_view_and_signal_check_missing_token_before_lookup() {
        let handle = route_handle(Arc::new(WebSessionManager::new()));

        let view_error = match handle
            .prepare_web_session_view_page("missing-session", None)
            .await
        {
            Ok(_) => panic!("missing view token should reject before lookup"),
            Err(error) => error,
        };
        assert_eq!(view_error, WebSessionAccessError::MissingToken);

        let signal_error = match handle
            .authorize_web_session_signal_bridge("missing-session", None)
            .await
        {
            Ok(_) => panic!("missing signal token should reject before lookup"),
            Err(error) => error,
        };
        assert_eq!(signal_error, WebSessionAccessError::MissingToken);
    }

    #[tokio::test]
    async fn web_session_present_invalid_token_reports_missing_session() {
        let handle = route_handle(Arc::new(WebSessionManager::new()));

        let view_error = match handle
            .prepare_web_session_view_page("missing-session", Some("bad-token"))
            .await
        {
            Ok(_) => panic!("missing session should reject after token presence check"),
            Err(error) => error,
        };
        assert_eq!(view_error, WebSessionAccessError::NotFound);

        let signal_error = match handle
            .authorize_web_session_signal_bridge("missing-session", Some("bad-token"))
            .await
        {
            Ok(_) => panic!("missing session should reject after token presence check"),
            Err(error) => error,
        };
        assert_eq!(signal_error, WebSessionAccessError::NotFound);
    }
}
