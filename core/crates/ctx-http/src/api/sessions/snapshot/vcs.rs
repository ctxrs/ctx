use super::*;
use ctx_route_contracts::sessions::{
    ApplySessionVcsDiffPatchRouteRequest, SessionRouteParams, SessionVcsDiffRouteResponse,
    SessionVcsDiffSummaryRouteResponse, SessionVcsGitStatusRouteResponse, SessionVcsRouteError,
    SessionVcsRouteErrorKind, SessionVcsRouteQuery,
};

#[path = "vcs/apply.rs"]
mod apply;
#[path = "vcs/diff.rs"]
mod diff;
#[path = "vcs/git_status.rs"]
mod git_status;

pub(crate) use apply::apply_session_diff_patch;
pub(crate) use diff::{get_session_diff, get_session_diff_summary};
pub(crate) use git_status::get_session_git_status;

fn session_vcs_status(error: &SessionVcsRouteError) -> StatusCode {
    match error.kind() {
        SessionVcsRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        SessionVcsRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        SessionVcsRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn session_vcs_api_error(error: SessionVcsRouteError) -> (StatusCode, Json<ApiErrorResp>) {
    let status = session_vcs_status(&error);
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
    use serde::de::DeserializeOwned;
    use serde_json::json;

    fn route_request<T: DeserializeOwned>(value: serde_json::Value) -> T {
        serde_json::from_value(value).unwrap()
    }

    async fn session_vcs_handle() -> SessionVcsHandle {
        crate::test_support::TestDaemonFixture::new("http://127.0.0.1:0")
            .await
            .daemon()
            .session_vcs_handle_for_test()
    }

    #[tokio::test]
    async fn vcs_read_routes_return_json_invalid_id_errors() {
        let err = get_session_diff(
            State(session_vcs_handle().await),
            Path("not-a-session".to_string()),
            Query(SessionVcsRouteQuery::default()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert_eq!(err.1 .0.error, "invalid session id");

        let err = get_session_diff_summary(
            State(session_vcs_handle().await),
            Path("not-a-session".to_string()),
            Query(SessionVcsRouteQuery::default()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert_eq!(err.1 .0.error, "invalid session id");

        let err = get_session_git_status(
            State(session_vcs_handle().await),
            Path("not-a-session".to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert_eq!(err.1 .0.error, "invalid session id");
    }

    #[tokio::test]
    async fn vcs_apply_route_returns_json_validation_errors() {
        let err = apply_session_diff_patch(
            State(session_vcs_handle().await),
            Path("not-a-session".to_string()),
            Json(route_request::<ApplySessionVcsDiffPatchRouteRequest>(
                json!({
                    "action": "accept",
                    "patch": "patch"
                }),
            )),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert_eq!(err.1 .0.error, "invalid session id");

        let valid_id = "00000000-0000-0000-0000-000000000001".to_string();
        let err = apply_session_diff_patch(
            State(session_vcs_handle().await),
            Path(valid_id.clone()),
            Json(route_request::<ApplySessionVcsDiffPatchRouteRequest>(
                json!({
                    "action": "bogus",
                    "patch": "   "
                }),
            )),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert_eq!(err.1 .0.error, "patch is empty");

        let err = apply_session_diff_patch(
            State(session_vcs_handle().await),
            Path(valid_id),
            Json(route_request::<ApplySessionVcsDiffPatchRouteRequest>(
                json!({
                    "action": "bogus",
                    "patch": "patch"
                }),
            )),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert_eq!(err.1 .0.error, "action must be accept or reject");
    }
}
