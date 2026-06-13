use super::*;
use ctx_subagent_service::route_contract::{
    ArchiveAgentRouteRequest, ArchiveAgentRouteResponse, GetAgentRouteRequest,
    GetAgentRouteResponse, InterruptAgentRouteRequest, InterruptAgentRouteResponse,
    ListAgentsRouteResponse, SendInputRouteRequest, SendInputRouteResponse,
    SessionSubagentInvocationRouteResponse, SessionSubagentInvocationsRouteQuery,
    SessionSubagentInvocationsRouteResponse, SessionSubagentRouteError,
    SessionSubagentRouteErrorKind, SessionSubagentsRouteResponse, SpawnAgentRouteRequest,
    SpawnAgentRouteResponse, WaitAgentRouteRequest, WaitAgentRouteResponse,
};

mod handlers;
mod init;
mod listings;

pub(crate) use handlers::*;
pub(crate) use init::*;
pub(crate) use listings::{
    get_session_subagent_invocation, list_session_subagent_invocations, list_session_subagents,
};

fn subagent_route_status(error: &SessionSubagentRouteError) -> StatusCode {
    match error.kind() {
        SessionSubagentRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        SessionSubagentRouteErrorKind::Unauthorized => StatusCode::UNAUTHORIZED,
        SessionSubagentRouteErrorKind::Forbidden => StatusCode::FORBIDDEN,
        SessionSubagentRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        SessionSubagentRouteErrorKind::InsufficientStorage => StatusCode::INSUFFICIENT_STORAGE,
        SessionSubagentRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn subagent_bare_status(error: SessionSubagentRouteError) -> StatusCode {
    subagent_route_status(&error)
}

fn subagent_api_error(error: SessionSubagentRouteError) -> (StatusCode, Json<ApiErrorResp>) {
    let status = subagent_route_status(&error);
    (
        status,
        Json(ApiErrorResp {
            error: error.message().to_string(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDaemonFixture;
    use ctx_core::ids::SessionId;
    use serde::de::DeserializeOwned;
    use serde_json::json;

    async fn sessions_fixture() -> TestDaemonFixture {
        TestDaemonFixture::new("http://127.0.0.1:0").await
    }

    fn route_request<T: DeserializeOwned>(value: serde_json::Value) -> T {
        serde_json::from_value(value).unwrap()
    }

    #[tokio::test]
    async fn public_listing_routes_preserve_bare_status_errors() {
        let fixture = sessions_fixture().await;
        let handle = fixture.daemon().session_subagent_read_handle_for_test();

        assert_eq!(
            list_session_subagents(State(handle.clone()), Path("not-a-session".to_string()))
                .await
                .unwrap_err(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            list_session_subagent_invocations(
                State(handle.clone()),
                Path("not-a-session".to_string()),
                Query(SessionSubagentInvocationsRouteQuery::default()),
            )
            .await
            .unwrap_err(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            get_session_subagent_invocation(
                State(handle.clone()),
                Path(("not-a-session".to_string(), "invocation".to_string())),
            )
            .await
            .unwrap_err(),
            StatusCode::BAD_REQUEST
        );

        let missing = SessionId::new().0.to_string();
        assert_eq!(
            list_session_subagents(State(handle.clone()), Path(missing.clone()))
                .await
                .unwrap_err(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            list_session_subagent_invocations(
                State(handle.clone()),
                Path(missing.clone()),
                Query(SessionSubagentInvocationsRouteQuery::default()),
            )
            .await
            .unwrap_err(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            get_session_subagent_invocation(
                State(handle),
                Path((missing, "invocation".to_string())),
            )
            .await
            .unwrap_err(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn mcp_subagent_routes_preserve_json_invalid_id_errors() {
        let fixture = sessions_fixture().await;
        let handle = fixture
            .daemon()
            .session_subagent_mcp_control_handle_for_test();
        let mcp_read_handle = fixture.daemon().session_subagent_mcp_read_handle_for_test();

        let err = mcp_spawn_agent(
            State(handle.clone()),
            None,
            Path("not-a-session".to_string()),
            Json(route_request::<SpawnAgentRouteRequest>(json!({
                "task_label": "child",
                "prompt": "work"
            }))),
        )
        .await
        .unwrap_err();
        assert_json_invalid_session_id(err);

        let err = mcp_send_input(
            State(handle.clone()),
            None,
            Path("not-a-session".to_string()),
            Json(route_request::<SendInputRouteRequest>(json!({
                "agent_id": "agent",
                "message": "hello"
            }))),
        )
        .await
        .unwrap_err();
        assert_json_invalid_session_id(err);

        let err = mcp_archive_agent(
            State(handle.clone()),
            None,
            Path("not-a-session".to_string()),
            Json(route_request::<ArchiveAgentRouteRequest>(json!({
                "agent_id": "agent"
            }))),
        )
        .await
        .unwrap_err();
        assert_json_invalid_session_id(err);

        let err = mcp_interrupt_agent(
            State(handle.clone()),
            None,
            Path("not-a-session".to_string()),
            Json(route_request::<InterruptAgentRouteRequest>(json!({
                "agent_id": "agent"
            }))),
        )
        .await
        .unwrap_err();
        assert_json_invalid_session_id(err);

        let err = mcp_list_agents(
            State(mcp_read_handle.clone()),
            None,
            Path("not-a-session".to_string()),
        )
        .await
        .unwrap_err();
        assert_json_invalid_session_id(err);

        let err = mcp_get_agent(
            State(mcp_read_handle.clone()),
            None,
            Path("not-a-session".to_string()),
            Json(route_request::<GetAgentRouteRequest>(json!({
                "agent_id": "agent"
            }))),
        )
        .await
        .unwrap_err();
        assert_json_invalid_session_id(err);

        let err = mcp_wait_agent(
            State(mcp_read_handle),
            None,
            Path("not-a-session".to_string()),
            Json(route_request::<WaitAgentRouteRequest>(json!({
                "agent_id": "agent"
            }))),
        )
        .await
        .unwrap_err();
        assert_json_invalid_session_id(err);
    }

    fn assert_json_invalid_session_id(error: (StatusCode, Json<ApiErrorResp>)) {
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        assert_eq!(error.1 .0.error, "invalid session id");
    }
}
