use super::route_contract::parse_session_route_id;
use crate::daemon::sessions::{
    DemoSeedTranscript, DemoSeedTranscriptError, DemoSeedTranscriptHandle, DemoSeedTranscriptTurn,
};
use ctx_route_contracts::sessions::{
    DemoSeedTranscriptRouteError, DemoSeedTranscriptRouteRequest, DemoSeedTranscriptRouteResponse,
    SessionRouteParams,
};

impl DemoSeedTranscriptHandle {
    pub async fn seed_demo_transcript_for_route(
        &self,
        params: SessionRouteParams,
        request: DemoSeedTranscriptRouteRequest,
    ) -> Result<DemoSeedTranscriptRouteResponse, DemoSeedTranscriptRouteError> {
        let session_id = parse_session_route_id(params.session_id())
            .map_err(|_| DemoSeedTranscriptRouteError::bad_request("invalid session id"))?;
        let seed = demo_seed_transcript_request_into_seed(request)?;
        let result = self
            .seed_demo_transcript(session_id, seed)
            .await
            .map_err(demo_seed_transcript_route_error)?;
        Ok(DemoSeedTranscriptRouteResponse {
            session_id: session_id.0.to_string(),
            seeded_turns: result.seeded_turns,
            seeded_messages: result.seeded_messages,
            seeded_events: result.seeded_events,
        })
    }
}

fn demo_seed_transcript_request_into_seed(
    request: DemoSeedTranscriptRouteRequest,
) -> Result<DemoSeedTranscript, DemoSeedTranscriptRouteError> {
    let (session_title, task_title, append, refresh, materialize_tail_turns, turns) =
        request.into_parts();
    if turns.is_empty() {
        return Err(DemoSeedTranscriptRouteError::bad_request(
            "turns must not be empty",
        ));
    }
    Ok(DemoSeedTranscript {
        session_title,
        task_title,
        append,
        refresh,
        materialize_tail_turns,
        turns: turns
            .into_iter()
            .map(|turn| {
                let (user, assistant, context_window) = turn.into_parts();
                DemoSeedTranscriptTurn {
                    user,
                    assistant,
                    context_window,
                }
            })
            .collect(),
    })
}

fn demo_seed_transcript_route_error(
    error: DemoSeedTranscriptError,
) -> DemoSeedTranscriptRouteError {
    match error {
        DemoSeedTranscriptError::SessionNotFound => {
            DemoSeedTranscriptRouteError::not_found("session not found")
        }
        DemoSeedTranscriptError::SessionAlreadyHasMessages => {
            DemoSeedTranscriptRouteError::conflict(
                "session already has messages; seed into a fresh session",
            )
        }
        DemoSeedTranscriptError::StoreUnavailable => {
            DemoSeedTranscriptRouteError::internal("failed to load session")
        }
        DemoSeedTranscriptError::InspectMessages => {
            DemoSeedTranscriptRouteError::internal("failed to inspect session messages")
        }
        DemoSeedTranscriptError::UpdateSessionTitle => {
            DemoSeedTranscriptRouteError::internal("failed to update session title")
        }
        DemoSeedTranscriptError::UpdateTaskTitle => {
            DemoSeedTranscriptRouteError::internal("failed to update task title")
        }
        DemoSeedTranscriptError::ReloadSession => {
            DemoSeedTranscriptRouteError::internal("failed to reload session")
        }
        DemoSeedTranscriptError::InsertUserMessage => {
            DemoSeedTranscriptRouteError::internal("failed to insert user message")
        }
        DemoSeedTranscriptError::InsertAssistantMessage => {
            DemoSeedTranscriptRouteError::internal("failed to insert assistant message")
        }
        DemoSeedTranscriptError::InsertSessionTurn => {
            DemoSeedTranscriptRouteError::internal("failed to insert session turn")
        }
        DemoSeedTranscriptError::AppendUserEvent => {
            DemoSeedTranscriptRouteError::internal("failed to append user event")
        }
        DemoSeedTranscriptError::AppendTurnStartedEvent => {
            DemoSeedTranscriptRouteError::internal("failed to append turn started event")
        }
        DemoSeedTranscriptError::AppendAssistantEvent => {
            DemoSeedTranscriptRouteError::internal("failed to append assistant event")
        }
        DemoSeedTranscriptError::AppendDoneEvent => {
            DemoSeedTranscriptRouteError::internal("failed to append done event")
        }
        DemoSeedTranscriptError::AppendTurnFinishedEvent => {
            DemoSeedTranscriptRouteError::internal("failed to append turn finished event")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_route_contracts::sessions::DemoSeedTranscriptRouteErrorKind;

    #[test]
    fn route_request_requires_turns() {
        let request: DemoSeedTranscriptRouteRequest = serde_json::from_value(serde_json::json!({
            "turns": []
        }))
        .expect("empty route request");

        let error = match demo_seed_transcript_request_into_seed(request) {
            Ok(_) => panic!("empty seed transcript request should fail"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), DemoSeedTranscriptRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "turns must not be empty");
    }

    #[test]
    fn route_error_maps_domain_errors() {
        let error = demo_seed_transcript_route_error(DemoSeedTranscriptError::SessionNotFound);
        assert_eq!(error.kind(), DemoSeedTranscriptRouteErrorKind::NotFound);
        assert_eq!(error.message(), "session not found");

        let error =
            demo_seed_transcript_route_error(DemoSeedTranscriptError::SessionAlreadyHasMessages);
        assert_eq!(error.kind(), DemoSeedTranscriptRouteErrorKind::Conflict);
        assert_eq!(
            error.message(),
            "session already has messages; seed into a fresh session"
        );
    }
}
