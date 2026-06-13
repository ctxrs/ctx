use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct DemoSeedTranscriptRouteTurn {
    user: String,
    assistant: String,
    #[serde(default)]
    context_window: Option<serde_json::Value>,
}

impl DemoSeedTranscriptRouteTurn {
    pub fn into_parts(self) -> (String, String, Option<serde_json::Value>) {
        (self.user, self.assistant, self.context_window)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DemoSeedTranscriptRouteRequest {
    #[serde(default)]
    session_title: Option<String>,
    #[serde(default)]
    task_title: Option<String>,
    #[serde(default)]
    append: bool,
    #[serde(default = "default_demo_seed_transcript_refresh")]
    refresh: bool,
    #[serde(default)]
    materialize_tail_turns: Option<usize>,
    turns: Vec<DemoSeedTranscriptRouteTurn>,
}

impl DemoSeedTranscriptRouteRequest {
    pub fn into_parts(
        self,
    ) -> (
        Option<String>,
        Option<String>,
        bool,
        bool,
        Option<usize>,
        Vec<DemoSeedTranscriptRouteTurn>,
    ) {
        (
            self.session_title,
            self.task_title,
            self.append,
            self.refresh,
            self.materialize_tail_turns,
            self.turns,
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DemoSeedTranscriptRouteResponse {
    pub session_id: String,
    pub seeded_turns: usize,
    pub seeded_messages: usize,
    pub seeded_events: usize,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DemoSeedTranscriptRouteErrorKind {
    BadRequest,
    NotFound,
    Conflict,
    Internal,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DemoSeedTranscriptRouteError {
    kind: DemoSeedTranscriptRouteErrorKind,
    message: &'static str,
}

impl DemoSeedTranscriptRouteError {
    pub fn new(kind: DemoSeedTranscriptRouteErrorKind, message: &'static str) -> Self {
        Self { kind, message }
    }

    pub fn bad_request(message: &'static str) -> Self {
        Self::new(DemoSeedTranscriptRouteErrorKind::BadRequest, message)
    }

    pub fn not_found(message: &'static str) -> Self {
        Self::new(DemoSeedTranscriptRouteErrorKind::NotFound, message)
    }

    pub fn conflict(message: &'static str) -> Self {
        Self::new(DemoSeedTranscriptRouteErrorKind::Conflict, message)
    }

    pub fn internal(message: &'static str) -> Self {
        Self::new(DemoSeedTranscriptRouteErrorKind::Internal, message)
    }

    pub fn kind(&self) -> DemoSeedTranscriptRouteErrorKind {
        self.kind
    }

    pub fn message(&self) -> &'static str {
        self.message
    }
}

fn default_demo_seed_transcript_refresh() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn demo_seed_request_preserves_defaults_and_turn_payloads() {
        let request: DemoSeedTranscriptRouteRequest = serde_json::from_value(json!({
            "session_title": "Demo",
            "turns": [{
                "user": "hello",
                "assistant": "hi",
                "context_window": {"used": 10}
            }]
        }))
        .expect("demo seed request");

        let (session_title, task_title, append, refresh, materialize_tail_turns, turns) =
            request.into_parts();
        assert_eq!(session_title.as_deref(), Some("Demo"));
        assert_eq!(task_title, None);
        assert!(!append);
        assert!(refresh);
        assert_eq!(materialize_tail_turns, None);
        assert_eq!(turns.len(), 1);

        let (user, assistant, context_window) = turns.into_iter().next().unwrap().into_parts();
        assert_eq!(user, "hello");
        assert_eq!(assistant, "hi");
        assert_eq!(context_window, Some(json!({"used": 10})));
    }

    #[test]
    fn demo_seed_response_preserves_wire_shape() {
        let response = DemoSeedTranscriptRouteResponse {
            session_id: "session-id".to_string(),
            seeded_turns: 2,
            seeded_messages: 4,
            seeded_events: 8,
        };

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({
                "session_id": "session-id",
                "seeded_turns": 2,
                "seeded_messages": 4,
                "seeded_events": 8,
            })
        );
    }

    #[test]
    fn demo_seed_route_errors_preserve_kind_and_message() {
        let error = DemoSeedTranscriptRouteError::conflict("already seeded");

        assert_eq!(error.kind(), DemoSeedTranscriptRouteErrorKind::Conflict);
        assert_eq!(error.message(), "already seeded");
    }
}
