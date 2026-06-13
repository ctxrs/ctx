use std::time::Instant;

use ctx_observability::logs;
use ctx_providers::ask_user_question::AskUserQuestionOutcome;
use ctx_route_contracts::sessions::{
    AuthenticateSessionRouteRequest, SessionControlRouteError, SessionFileCompletionsRouteQuery,
    SessionFileCompletionsRouteResponse, SessionRouteParams, SubmitAskUserQuestionRouteRequest,
    SubmitAskUserQuestionRouteResponse,
};

use crate::daemon::sessions::ask_user::{SubmitAskUserAnswer, SubmitAskUserAnswerError};
use crate::daemon::sessions::auth::SessionAuthError;
use crate::daemon::sessions::command_dispatch::SessionSchedulerCommandError;
use crate::daemon::sessions::route_contract::parse_session_route_id;
use crate::daemon::workspaces::FileCompletionsErrorKind;
use crate::daemon::{SessionControlHandle, SessionFileCompletionsHandle};

impl SessionControlHandle {
    pub async fn cancel_session_for_route(
        &self,
        params: SessionRouteParams,
    ) -> Result<(), SessionControlRouteError> {
        let session_id = parse_control_session_id(params)?;
        self.cancel_session(session_id)
            .await
            .map_err(scheduler_command_error)
    }

    pub async fn interrupt_session_for_route(
        &self,
        params: SessionRouteParams,
        request_started: Instant,
    ) -> Result<(), SessionControlRouteError> {
        let session_id = parse_control_session_id(params)?;
        self.interrupt_session(session_id, request_started)
            .await
            .map_err(scheduler_command_error)
    }

    pub async fn authenticate_session_for_route(
        &self,
        params: SessionRouteParams,
        request: AuthenticateSessionRouteRequest,
    ) -> Result<(), SessionControlRouteError> {
        let session_id = parse_control_session_id(params)?;
        self.authenticate_session(session_id, request.into_method_id())
            .await
            .map_err(session_auth_error)
    }

    pub async fn submit_ask_user_question_for_route(
        &self,
        params: SessionRouteParams,
        request: SubmitAskUserQuestionRouteRequest,
    ) -> Result<SubmitAskUserQuestionRouteResponse, SessionControlRouteError> {
        let session_id = parse_control_session_id(params)?;
        self.submit_ask_user_answer(
            session_id,
            submit_ask_user_answer_from_route_request(request)?,
        )
        .await
        .map_err(submit_ask_user_answer_error)?;

        Ok(SubmitAskUserQuestionRouteResponse::ok())
    }
}

impl SessionFileCompletionsHandle {
    pub async fn complete_files_for_session_for_route(
        &self,
        params: SessionRouteParams,
        query: SessionFileCompletionsRouteQuery,
    ) -> Result<SessionFileCompletionsRouteResponse, SessionControlRouteError> {
        let session_id = parse_control_session_id(params)?;
        let (query, limit) = query.into_parts();
        self.complete_files_for_session(session_id, query, limit)
            .await
            .map(SessionFileCompletionsRouteResponse::new)
            .map_err(file_completions_error)
    }
}

fn submit_ask_user_answer_from_route_request(
    request: SubmitAskUserQuestionRouteRequest,
) -> Result<SubmitAskUserAnswer, SessionControlRouteError> {
    let (tool_call_id, outcome, answers) = request.into_parts();
    let outcome = match outcome.as_deref() {
        Some("cancelled") => AskUserQuestionOutcome::Cancelled,
        Some("submitted") | None => AskUserQuestionOutcome::Submitted,
        Some(other) => {
            return Err(SessionControlRouteError::bad_request(format!(
                "invalid outcome: {other}"
            )));
        }
    };

    Ok(SubmitAskUserAnswer {
        tool_call_id,
        outcome,
        answers: answers.unwrap_or_default(),
    })
}

fn parse_control_session_id(
    params: SessionRouteParams,
) -> Result<ctx_core::ids::SessionId, SessionControlRouteError> {
    parse_session_route_id(params.session_id())
        .map_err(|_| SessionControlRouteError::bad_request("invalid session id"))
}

fn scheduler_command_error(error: SessionSchedulerCommandError) -> SessionControlRouteError {
    match error {
        SessionSchedulerCommandError::BadRequest => {
            SessionControlRouteError::bad_request("bad request")
        }
        SessionSchedulerCommandError::NotFound => {
            SessionControlRouteError::not_found("session not found")
        }
        SessionSchedulerCommandError::StoreUnavailable => {
            SessionControlRouteError::internal("session store unavailable")
        }
    }
}

fn session_auth_error(error: SessionAuthError) -> SessionControlRouteError {
    match error {
        SessionAuthError::NotFound(entity) => {
            SessionControlRouteError::not_found(format!("{entity} not found"))
        }
        SessionAuthError::BadRequest(error) => SessionControlRouteError::bad_request(error),
        SessionAuthError::Forbidden(error) => SessionControlRouteError::forbidden(error),
        SessionAuthError::Internal(error) => SessionControlRouteError::internal(error),
        SessionAuthError::AuthenticationFailed { redacted_message } => {
            tracing::warn!("session authentication failed: {redacted_message}");
            SessionControlRouteError::bad_request("authentication failed")
        }
    }
}

fn submit_ask_user_answer_error(error: SubmitAskUserAnswerError) -> SessionControlRouteError {
    match error {
        SubmitAskUserAnswerError::MissingToolCallId => {
            SessionControlRouteError::bad_request("missing tool_call_id")
        }
        SubmitAskUserAnswerError::SessionNotFound => {
            SessionControlRouteError::not_found("session not found")
        }
        SubmitAskUserAnswerError::StoreUnavailable(err) => {
            SessionControlRouteError::internal(logs::redact_sensitive(&err.to_string()))
        }
        SubmitAskUserAnswerError::LoadSession => {
            SessionControlRouteError::internal("failed to load session")
        }
        SubmitAskUserAnswerError::NoPendingQuestion => {
            SessionControlRouteError::conflict("no pending AskUserQuestion for this tool_call_id")
        }
    }
}

fn file_completions_error(
    error: crate::daemon::workspaces::FileCompletionsError,
) -> SessionControlRouteError {
    match error.kind() {
        FileCompletionsErrorKind::NotFound => {
            SessionControlRouteError::not_found(error.message().to_string())
        }
        FileCompletionsErrorKind::Forbidden => {
            SessionControlRouteError::forbidden(error.message().to_string())
        }
        FileCompletionsErrorKind::InsufficientStorage => {
            SessionControlRouteError::insufficient_storage(error.message().to_string())
        }
        FileCompletionsErrorKind::Internal => {
            tracing::warn!(error = error.message(), "file completions request failed");
            SessionControlRouteError::internal(error.message().to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_route_contracts::sessions::SessionControlRouteErrorKind;
    use serde_json::json;

    async fn session_handles_for_test() -> (
        tempfile::TempDir,
        SessionControlHandle,
        SessionFileCompletionsHandle,
    ) {
        let temp = tempfile::tempdir().expect("tempdir");
        let daemon = crate::test_support::TestDaemon::new_for_test(
            temp.path().join("data"),
            "http://127.0.0.1:4310".to_string(),
        )
        .await
        .expect("daemon");
        (
            temp,
            daemon.session_control_handle_for_test(),
            daemon.session_file_completions_handle_for_test(),
        )
    }

    #[tokio::test]
    async fn migrated_control_handle_routes_parse_ids_before_effects() {
        let (_temp, control, file_completions) = session_handles_for_test().await;

        let cancel = control
            .cancel_session_for_route(SessionRouteParams::new("not-a-session"))
            .await
            .expect_err("invalid cancel route id");
        assert_eq!(cancel.kind(), SessionControlRouteErrorKind::BadRequest);
        assert_eq!(cancel.message(), "invalid session id");

        let completions = file_completions
            .complete_files_for_session_for_route(
                SessionRouteParams::new("not-a-session"),
                SessionFileCompletionsRouteQuery::default(),
            )
            .await
            .expect_err("invalid file-completion route id");
        assert_eq!(completions.kind(), SessionControlRouteErrorKind::BadRequest);
        assert_eq!(completions.message(), "invalid session id");
    }

    #[test]
    fn ask_user_request_adapter_defaults_and_validates_outcome() {
        let submitted = submit_ask_user_answer_from_route_request(
            serde_json::from_value(json!({"tool_call_id": "tool-1"})).unwrap(),
        )
        .unwrap();
        assert_eq!(submitted.tool_call_id, "tool-1");
        assert!(matches!(
            submitted.outcome,
            AskUserQuestionOutcome::Submitted
        ));
        assert!(submitted.answers.is_empty());

        let cancelled = submit_ask_user_answer_from_route_request(
            serde_json::from_value(json!({
                "tool_call_id": "tool-1",
                "outcome": "cancelled",
                "answers": {"choice": "no"}
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(matches!(
            cancelled.outcome,
            AskUserQuestionOutcome::Cancelled
        ));
        assert_eq!(
            cancelled.answers.get("choice").map(String::as_str),
            Some("no")
        );

        let invalid = submit_ask_user_answer_from_route_request(
            serde_json::from_value(json!({
                "tool_call_id": "tool-1",
                "outcome": "declined"
            }))
            .unwrap(),
        )
        .unwrap_err();
        assert_eq!(invalid.kind(), SessionControlRouteErrorKind::BadRequest);
        assert_eq!(invalid.message(), "invalid outcome: declined");
    }

    #[test]
    fn control_route_errors_classify_existing_status_categories() {
        assert_eq!(
            scheduler_command_error(SessionSchedulerCommandError::BadRequest).kind(),
            SessionControlRouteErrorKind::BadRequest
        );
        assert_eq!(
            scheduler_command_error(SessionSchedulerCommandError::NotFound).kind(),
            SessionControlRouteErrorKind::NotFound
        );
        assert_eq!(
            scheduler_command_error(SessionSchedulerCommandError::StoreUnavailable).kind(),
            SessionControlRouteErrorKind::Internal
        );
    }

    #[test]
    fn auth_errors_preserve_user_facing_messages() {
        let not_found = session_auth_error(SessionAuthError::NotFound("session"));
        assert_eq!(not_found.kind(), SessionControlRouteErrorKind::NotFound);
        assert_eq!(not_found.message(), "session not found");

        let forbidden = session_auth_error(SessionAuthError::Forbidden(
            "host execution is disabled by daemon policy".to_string(),
        ));
        assert_eq!(forbidden.kind(), SessionControlRouteErrorKind::Forbidden);
        assert_eq!(
            forbidden.message(),
            "host execution is disabled by daemon policy"
        );

        let failed = session_auth_error(SessionAuthError::AuthenticationFailed {
            redacted_message: "provider rejected credentials".to_string(),
        });
        assert_eq!(failed.kind(), SessionControlRouteErrorKind::BadRequest);
        assert_eq!(failed.message(), "authentication failed");
    }

    #[test]
    fn ask_user_errors_preserve_user_facing_messages() {
        let missing = submit_ask_user_answer_error(SubmitAskUserAnswerError::MissingToolCallId);
        assert_eq!(missing.kind(), SessionControlRouteErrorKind::BadRequest);
        assert_eq!(missing.message(), "missing tool_call_id");

        let conflict = submit_ask_user_answer_error(SubmitAskUserAnswerError::NoPendingQuestion);
        assert_eq!(conflict.kind(), SessionControlRouteErrorKind::Conflict);
        assert_eq!(
            conflict.message(),
            "no pending AskUserQuestion for this tool_call_id"
        );
    }

    #[test]
    fn session_id_parser_reuses_route_contract_semantics() {
        let error = parse_control_session_id(SessionRouteParams::new("not-a-session")).unwrap_err();
        assert_eq!(error.kind(), SessionControlRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "invalid session id");
    }
}
