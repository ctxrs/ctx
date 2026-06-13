use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEventType {
    Init,
    UserMessage,
    InputQueued,
    TurnQueued,
    TurnStarted,
    ContextWindowUpdate,
    TurnFinished,
    AuthRequired,
    Notice,
    AssistantChunk,
    ThoughtChunk,
    AssistantComplete,
    AssistantMessageInserted,
    ToolCall,
    ToolCallUpdate,
    ToolResult,
    Plan,
    ArtifactsSet,
    Done,
    InterruptRequested,
    TurnInterrupted,
    MessageQueueAdded,
    MessageQueueUpdated,
    MessageQueueRemoved,
    MessageQueuePromoted,
    Error,
}

#[derive(Debug, Clone)]
pub struct SessionEvent {
    pub seq: i64,
    pub id: SessionEventId,
    pub session_id: SessionId,
    pub run_id: Option<RunId>,
    pub turn_id: Option<TurnId>,
    pub event_type: SessionEventType,
    pub payload_json: serde_json::Value,
    pub transient: bool,
    pub created_at: DateTime<Utc>,
}

impl<'de> Deserialize<'de> for SessionEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            #[serde(default)]
            seq: Option<i64>,
            id: SessionEventId,
            session_id: SessionId,
            #[serde(default)]
            run_id: Option<RunId>,
            #[serde(default)]
            turn_id: Option<TurnId>,
            event_type: SessionEventType,
            payload_json: serde_json::Value,
            #[serde(default)]
            transient: bool,
            created_at: DateTime<Utc>,
        }

        let wire = Wire::deserialize(deserializer)?;
        // For stream-only/transient events we serialize `seq` as null. Allow
        // deserializers (including Rust tests) to accept this by defaulting.
        let transient = wire.transient || wire.seq.is_none();
        Ok(SessionEvent {
            seq: wire.seq.unwrap_or(0),
            id: wire.id,
            session_id: wire.session_id,
            run_id: wire.run_id,
            turn_id: wire.turn_id,
            event_type: wire.event_type,
            payload_json: wire.payload_json,
            transient,
            created_at: wire.created_at,
        })
    }
}

impl Serialize for SessionEvent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        let mut state = serializer.serialize_struct("SessionEvent", 9)?;
        if self.transient {
            state.serialize_field("seq", &Option::<i64>::None)?;
        } else {
            state.serialize_field("seq", &self.seq)?;
        }
        state.serialize_field("id", &self.id)?;
        state.serialize_field("session_id", &self.session_id)?;
        state.serialize_field("run_id", &self.run_id)?;
        state.serialize_field("turn_id", &self.turn_id)?;
        state.serialize_field("event_type", &self.event_type)?;
        state.serialize_field("payload_json", &self.payload_json)?;
        if self.transient {
            state.serialize_field("transient", &true)?;
        }
        state.serialize_field("created_at", &self.created_at)?;
        state.end()
    }
}
