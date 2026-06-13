use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;

use super::errors::ApiErrorResp;
use ctx_daemon::daemon::{
    SessionControlHandle, SessionFileCompletionsHandle, SessionMessageCommandHandle,
    SessionReadModelsHandle, SessionSubagentMcpControlHandle, SessionSubagentMcpReadHandle,
    SessionSubagentReadHandle, SessionVcsHandle,
};
use ctx_route_contracts::sessions::{
    AuthenticateSessionRouteRequest, DeleteSessionMessageRouteParams,
    PostSessionMessageRouteRequest, PostSessionMessageRouteResponse, SessionControlRouteError,
    SessionControlRouteErrorKind, SessionEventsRouteQuery, SessionEventsRouteResponse,
    SessionFileCompletionsRouteQuery, SessionHeadRouteQuery, SessionHeadRouteResponse,
    SessionHistoryRouteQuery, SessionHistoryRouteResponse, SessionMessageRouteError,
    SessionMessageRouteErrorKind, SessionReadModelRouteError, SessionReadModelRouteErrorKind,
    SessionRouteParams, SessionSnapshotRouteQuery, SessionSnapshotRouteResponse,
    SessionStateRouteResponse, SessionTurnToolsRouteParams, SessionTurnToolsRouteResponse,
    SubmitAskUserQuestionRouteRequest,
};
#[cfg(test)]
use ctx_settings_model as user_settings;

mod subagents;
pub(super) use subagents::{
    get_session_subagent_invocation, list_session_subagent_invocations, list_session_subagents,
    mcp_archive_agent, mcp_get_agent, mcp_interrupt_agent, mcp_list_agents, mcp_send_input,
    mcp_spawn_agent, mcp_wait_agent,
};
mod control;
pub(super) use control::{
    authenticate_session, cancel_session, interrupt_session, submit_ask_user_question,
};
mod file_completions;
pub(super) use file_completions::session_file_completions;
mod messages;
pub(super) use messages::{delete_session_message, post_message};
mod snapshot;
pub(super) use snapshot::{
    apply_session_diff_patch, get_session_diff, get_session_diff_summary, get_session_events,
    get_session_git_status, get_session_head, get_session_history, get_session_snapshot,
    get_session_state, list_session_turn_tools,
};
mod titles_and_modes;
#[cfg(test)]
use ctx_session_title_service::title_generation;
pub(super) use titles_and_modes::{generate_session_title, set_session_mode, set_session_model};

#[cfg(test)]
mod tests;

fn session_read_model_status(error: SessionReadModelRouteError) -> StatusCode {
    match error.kind() {
        SessionReadModelRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        SessionReadModelRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        SessionReadModelRouteErrorKind::Conflict => StatusCode::CONFLICT,
        SessionReadModelRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn session_control_status(error: &SessionControlRouteError) -> StatusCode {
    match error.kind() {
        SessionControlRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        SessionControlRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        SessionControlRouteErrorKind::Forbidden => StatusCode::FORBIDDEN,
        SessionControlRouteErrorKind::Conflict => StatusCode::CONFLICT,
        SessionControlRouteErrorKind::InsufficientStorage => StatusCode::INSUFFICIENT_STORAGE,
        SessionControlRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn session_control_bare_status(error: SessionControlRouteError) -> StatusCode {
    session_control_status(&error)
}

fn session_control_api_error(error: SessionControlRouteError) -> (StatusCode, Json<ApiErrorResp>) {
    let status = session_control_status(&error);
    (
        status,
        Json(ApiErrorResp {
            error: error.message().to_string(),
        }),
    )
}

fn session_message_status(error: &SessionMessageRouteError) -> StatusCode {
    match error.kind() {
        SessionMessageRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        SessionMessageRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        SessionMessageRouteErrorKind::Conflict => StatusCode::CONFLICT,
        SessionMessageRouteErrorKind::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        SessionMessageRouteErrorKind::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
        SessionMessageRouteErrorKind::ServiceUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        SessionMessageRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn session_message_bare_status(error: SessionMessageRouteError) -> StatusCode {
    session_message_status(&error)
}

fn session_message_api_error(error: SessionMessageRouteError) -> (StatusCode, Json<ApiErrorResp>) {
    let status = session_message_status(&error);
    (
        status,
        Json(ApiErrorResp {
            error: error.message().to_string(),
        }),
    )
}
